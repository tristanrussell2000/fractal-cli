//! The per-profile package allowlist: may this profile mutate this object?
//!
//! The customer-namespace check authorizes an object's *name*. This authorizes
//! where the object *lives*, which is what a developer actually reasons about:
//! "my project's packages" rather than "names beginning with Z".
//!
//! Deliberately the object's **direct** package, with no walk up the package
//! hierarchy. On a real system, packages sharing a project's prefix but sitting
//! outside that project's package tree turned out to be root-level packages
//! with no parent at all — and one of them held live code. Naming tracked
//! intent better than the maintained hierarchy did, so a pattern on the direct
//! package is both cheaper and more accurate than walking parents.
//!
//! This is a seatbelt, not a security boundary. It checks where an object is
//! *right now*, so moving an object into a granted package and then editing it
//! defeats it. That is an acceptable trade for per-user config on a developer's
//! own machine, and it must not be described as access control.

use thiserror::Error;

use super::{
    adt_response::{AdtResponseParseError, parse_adt_document},
    client::{SapClient, SapClientError},
    find_child, find_non_empty_attribute,
};
use crate::config::{EditPolicy, TEMPORARY_PACKAGE};
use crate::pattern::glob_matches;
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::suggested_command;

/// A refusal, or a failure to establish where an object lives.
#[derive(Debug, Error)]
pub enum PackageAuthorizationError {
    #[error("object '{name}' is in package {package}, which this profile may not edit")]
    PackageNotAllowed {
        name: String,
        package: String,
        patterns: Vec<String>,
    },
    #[error("object '{name}' does not report a package, so it cannot be authorized")]
    PackageUnknown { name: String, object_uri: String },
    #[error("could not read the package of object '{name}': {source}")]
    PackageLookup {
        name: String,
        #[source]
        source: Box<SapClientError>,
    },
    #[error("could not parse the metadata of object '{name}': {source}")]
    Parse {
        name: String,
        #[source]
        source: AdtResponseParseError,
    },
}

impl PackageAuthorizationError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            // Explicit reborrow rather than deref coercion: this is `const`
            // because callers in other error types are.
            Self::PackageLookup { source, .. } => Some(&**source),
            _ => None,
        }
    }
}

impl ReportableError for PackageAuthorizationError {
    fn code(&self) -> &'static str {
        match self {
            Self::PackageNotAllowed { .. } => "object_outside_edit_packages",
            Self::PackageUnknown { .. } => "object_package_unknown",
            Self::PackageLookup { .. } => "object_package_lookup_failed",
            Self::Parse { .. } => "adt_response_parse_error",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::PackageNotAllowed {
                package, patterns, ..
            } => {
                if patterns.is_empty() {
                    "This profile grants no packages at all, so nothing is editable. Grant some with `fractal auth set --package <pattern>`."
                        .to_owned()
                } else {
                    format!(
                        "This profile may edit packages matching {}. The object is in {package}. Change the object, or grant its package with `fractal auth set --package <pattern>`.",
                        patterns.join(", ")
                    )
                }
            }
            // Fail closed: an unproven package is not an authorized one.
            Self::PackageUnknown { .. } => {
                "The object's metadata carries no package, so the package allowlist cannot authorize it. Check the object with `fractal object xml`."
                    .to_owned()
            }
            Self::PackageLookup { source, .. } => source.hint()?,
            Self::Parse { source, .. } => source.hint()?,
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            // Show the caller the metadata that failed to name a package.
            Self::PackageUnknown { object_uri, .. } => {
                Some(suggested_command::object_xml(object_uri))
            }
            // Nothing to suggest: the remedy is `fractal auth set`, which
            // changes configuration, and a suggested command may be run
            // directly, so it must never mutate anything. The hint says it.
            Self::PackageNotAllowed { .. } | Self::Parse { .. } => None,
            Self::PackageLookup { source, .. } => source.suggested_command(),
        }
    }
}

/// Whether a package is granted, without needing a request.
///
/// `$TMP` is the per-user scratch package: objects in it are local, never
/// transported, and not shared code, so an allowlist that blocked them would
/// mostly be in the way. It stays granted unless the profile turns it off.
#[must_use]
pub fn package_is_allowed(policy: &EditPolicy, package: &str) -> bool {
    let Some(patterns) = policy.edit_packages.as_ref() else {
        return true;
    };
    if policy.allow_temporary_package && package.eq_ignore_ascii_case(TEMPORARY_PACKAGE) {
        return true;
    }
    patterns
        .iter()
        .any(|pattern| glob_matches(pattern, package))
}

/// Authorizes a package that the caller already knows.
///
/// The free path: `edit create` is given its package as an argument, and
/// `edit set-xml` reads the object before writing it, so neither needs a
/// request to find out.
///
/// # Errors
///
/// Returns [`PackageAuthorizationError::PackageNotAllowed`] when the profile
/// restricts packages and this one does not match.
pub fn authorize_known_package(
    policy: &EditPolicy,
    name: &str,
    package: &str,
) -> Result<(), PackageAuthorizationError> {
    if package_is_allowed(policy, package) {
        return Ok(());
    }
    Err(PackageAuthorizationError::PackageNotAllowed {
        name: name.to_owned(),
        package: package.to_owned(),
        patterns: policy.edit_packages.clone().unwrap_or_default(),
    })
}

/// Authorizes an object by reading which package it is in.
///
/// Costs one GET, and only when the profile actually restricts packages — an
/// unrestricted profile makes no request at all, so the guard is free for
/// everyone who has not opted in.
///
/// Call this *before* taking a lock, so a refusal never leaves one behind.
///
/// # Errors
///
/// Returns [`PackageAuthorizationError::PackageNotAllowed`] for a package
/// outside the allowlist, [`PackageAuthorizationError::PackageUnknown`] when
/// the object reports no package, or the lookup or parse failure that stopped
/// the check. Every one of those refuses the edit: an unproven package is not
/// an authorized package.
pub async fn authorize_object_package(
    sap: &SapClient,
    policy: &EditPolicy,
    name: &str,
    object_uri: &str,
) -> Result<(), PackageAuthorizationError> {
    if !policy.restricts_packages() {
        return Ok(());
    }

    let xml = sap.get_text(object_uri).await.map_err(|source| {
        PackageAuthorizationError::PackageLookup {
            name: name.to_owned(),
            source: Box::new(source),
        }
    })?;
    let package =
        package_of_object_xml(&xml).map_err(|source| PackageAuthorizationError::Parse {
            name: name.to_owned(),
            source,
        })?;
    let package = package.ok_or_else(|| PackageAuthorizationError::PackageUnknown {
        name: name.to_owned(),
        object_uri: object_uri.to_owned(),
    })?;

    authorize_known_package(policy, name, &package)
}

/// Reads the package name out of an object's metadata XML.
///
/// Every ADT family carries the same element — verified live on classes, DDL
/// sources, data elements, domains and a `$TMP` program. Read `adtcore:name`
/// rather than the URI: DDLS adds a redundant `adtcore:packageName`, and the
/// URI is percent-encoded (`$TMP` appears there as `%24tmp`).
///
/// # Errors
///
/// Returns [`AdtResponseParseError`] when the response is not valid XML. A
/// well-formed document with no `packageRef` is `Ok(None)`, which the caller
/// treats as unproven rather than as a parse failure.
pub fn package_of_object_xml(xml: &str) -> Result<Option<String>, AdtResponseParseError> {
    let document = parse_adt_document(xml)?;
    let Some(package_ref) = find_child(document.root_element(), "packageRef") else {
        return Ok(None);
    };
    Ok(find_non_empty_attribute(package_ref, "name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(packages: Option<Vec<&str>>) -> EditPolicy {
        EditPolicy {
            customer_namespaces: vec!["Z*".to_owned()],
            edit_packages: packages
                .map(|packages| packages.into_iter().map(str::to_owned).collect()),
            allow_temporary_package: true,
        }
    }

    #[test]
    fn an_unrestricted_profile_allows_every_package() {
        let policy = policy(None);
        assert!(package_is_allowed(&policy, "ZPROJ_CORE"));
        assert!(package_is_allowed(&policy, "SAPBASIS"));
        assert!(package_is_allowed(&policy, TEMPORARY_PACKAGE));
    }

    #[test]
    fn an_empty_list_allows_nothing_but_scratch() {
        let policy = policy(Some(vec![]));
        assert!(!package_is_allowed(&policy, "ZPROJ_CORE"));
        // The off switch is about shared code; local throwaway work still works
        // unless the profile turns that off too.
        assert!(package_is_allowed(&policy, TEMPORARY_PACKAGE));
    }

    #[test]
    fn patterns_match_the_direct_package_case_insensitively() {
        let policy = policy(Some(vec!["ZPROJ*", "ZUTIL"]));
        assert!(package_is_allowed(&policy, "ZPROJ_CORE"));
        assert!(package_is_allowed(&policy, "zproj_core"));
        assert!(package_is_allowed(&policy, "ZUTIL"));
        // Exact patterns stay exact.
        assert!(!package_is_allowed(&policy, "ZUTIL_EXTRA"));
        assert!(!package_is_allowed(&policy, "ZOTHER"));
    }

    #[test]
    fn scratch_access_can_be_turned_off() {
        let mut policy = policy(Some(vec!["ZPROJ*"]));
        policy.allow_temporary_package = false;
        assert!(!package_is_allowed(&policy, TEMPORARY_PACKAGE));
        assert!(package_is_allowed(&policy, "ZPROJ_CORE"));
    }

    #[test]
    fn a_refusal_names_the_package_and_the_patterns() {
        let error =
            authorize_known_package(&policy(Some(vec!["ZPROJ*"])), "ZCL_SAMPLE", "ZOTHER_PKG")
                .unwrap_err();

        assert_eq!(error.code(), "object_outside_edit_packages");
        let hint = error.hint().expect("has a hint");
        assert!(hint.contains("ZOTHER_PKG"), "{hint}");
        assert!(hint.contains("ZPROJ*"), "{hint}");
        // No suggested command at all: the remedy is `fractal auth set`, and a
        // suggested command may be run directly, so it must never mutate.
        assert_eq!(error.suggested_command(), None);
    }

    #[test]
    fn granting_nothing_says_so_rather_than_listing_no_patterns() {
        let error =
            authorize_known_package(&policy(Some(vec![])), "ZCL_SAMPLE", "ZPROJ_CORE").unwrap_err();
        let hint = error.hint().expect("has a hint");
        assert!(hint.contains("no packages at all"), "{hint}");
    }

    #[test]
    fn reads_the_package_name_from_object_metadata() {
        let xml = r#"<blue:wbobj xmlns:blue="urn:b" xmlns:adtcore="urn:a">
            <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zproj_core" adtcore:type="DEVC/K" adtcore:name="ZPROJ_CORE"/>
        </blue:wbobj>"#;
        assert_eq!(
            package_of_object_xml(xml).unwrap().as_deref(),
            Some("ZPROJ_CORE")
        );
    }

    #[test]
    fn reads_the_scratch_package_verbatim_rather_than_from_the_uri() {
        // Verified live: the URI percent-encodes the `$`, the name does not.
        let xml = r#"<program:abapProgram xmlns:program="urn:p" xmlns:adtcore="urn:a">
            <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/%24tmp" adtcore:type="DEVC/K" adtcore:name="$TMP"/>
        </program:abapProgram>"#;
        assert_eq!(package_of_object_xml(xml).unwrap().as_deref(), Some("$TMP"));
    }

    #[test]
    fn an_object_without_a_package_reference_is_unproven_not_malformed() {
        let xml = r#"<blue:wbobj xmlns:blue="urn:b"/>"#;
        assert_eq!(package_of_object_xml(xml).unwrap(), None);
    }

    #[test]
    fn malformed_metadata_is_a_parse_error() {
        assert!(package_of_object_xml("<not-closed").is_err());
    }
}
