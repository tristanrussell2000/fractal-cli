//! The caller's inactive-object worklist: which objects, and which class
//! includes, **this user** has staged.
//!
//! Per user, unlike the object's own document. It answers what an activation
//! will publish, because activation publishes the caller's worklist and leaves
//! other users' staged work alone. For what anyone has staged, see
//! [`super::staged_work`].

use thiserror::Error;

use super::{
    class_include::ClassInclude,
    client::{SapClient, SapClientError},
    find_attribute_value,
};
use crate::reportable_error::ReportableError;

const INACTIVE_OBJECTS_PATH: &str = "/sap/bc/adt/activation/inactiveobjects";

#[derive(Debug, Error)]
pub enum AdtInactiveSourceProbeError {
    #[error("SAP inactive-object lookup failed: {0}")]
    Sap(#[from] SapClientError),
    #[error("SAP returned malformed inactive-object XML: {0}")]
    Parse(#[from] roxmltree::Error),
}

impl AdtInactiveSourceProbeError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            Self::Parse(_) => None,
        }
    }

    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(_) => {
                Some("The SAP inactive-object response did not contain valid ADT XML.".to_owned())
            }
        }
    }
}

/// Whether the object is on the caller's worklist.
///
/// # Errors
///
/// Returns [`AdtInactiveSourceProbeError`] when the worklist cannot be read.
pub(super) async fn probe_inactive_adt_source(
    sap: &SapClient,
    object_uri: &str,
) -> Result<bool, AdtInactiveSourceProbeError> {
    read_worklist(sap).await?.contains(object_uri)
}

/// The caller's worklist, read once and asked several things.
pub(super) struct Worklist(String);

impl Worklist {
    /// Whether the object itself is on the worklist.
    pub(super) fn contains(&self, object_uri: &str) -> Result<bool, AdtInactiveSourceProbeError> {
        inactive_object_response_contains_uri(&self.0, object_uri)
    }

    /// Which of a class's includes the caller has staged: what activating the
    /// class will publish.
    ///
    /// An include is listed under its own URI; `main` is listed by section or
    /// method, as `source/main#type=…`.
    pub(super) fn staged_by_caller(
        &self,
        class_uri: &str,
    ) -> Result<Vec<ClassInclude>, AdtInactiveSourceProbeError> {
        staged_by_caller_in(&self.0, class_uri)
    }
}

/// # Errors
///
/// Returns [`AdtInactiveSourceProbeError`] when the worklist cannot be read.
pub(super) async fn read_worklist(
    sap: &SapClient,
) -> Result<Worklist, AdtInactiveSourceProbeError> {
    Ok(Worklist(sap.get_text(INACTIVE_OBJECTS_PATH).await?))
}

fn staged_by_caller_in(
    response: &str,
    class_uri: &str,
) -> Result<Vec<ClassInclude>, AdtInactiveSourceProbeError> {
    let document = roxmltree::Document::parse(response)?;
    let mut staged: Vec<ClassInclude> = document
        .descendants()
        .filter_map(|node| find_attribute_value(node, "uri"))
        .filter_map(ClassInclude::split_source_uri)
        .filter(|(class, _)| class.eq_ignore_ascii_case(class_uri))
        .map(|(_, include)| include)
        .collect();
    staged.sort_unstable();
    staged.dedup();
    Ok(staged)
}

fn inactive_object_response_contains_uri(
    response: &str,
    object_uri: &str,
) -> Result<bool, AdtInactiveSourceProbeError> {
    let document = roxmltree::Document::parse(response)?;
    Ok(document.descendants().any(|node| {
        node.attributes().any(|attribute| {
            attribute.name().eq_ignore_ascii_case("uri")
                && attribute.value().eq_ignore_ascii_case(object_uri)
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_which_includes_of_one_class_the_caller_staged() {
        let response = r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects" xmlns:adtcore="http://www.sap.com/adt/core">
            <ioc:entry><ioc:object/><ioc:transport/></ioc:entry>
            <ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample" adtcore:type="CLAS/OC"/></ioc:object></ioc:entry>
            <ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/source/main#type=CLAS%2FOSU;name=ZCL_SAMPLE" adtcore:type="CLAS/OSU"/></ioc:object></ioc:entry>
            <ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/includes/implementations" adtcore:type="CLAS/OCN/implementations" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample"/></ioc:object></ioc:entry>
            <ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample2/includes/macros" adtcore:type="CLAS/OCN/macros"/></ioc:object></ioc:entry>
        </ioc:inactiveObjects>"#;

        assert_eq!(
            staged_by_caller_in(response, "/sap/bc/adt/oo/classes/zcl_sample").unwrap(),
            vec![ClassInclude::Main, ClassInclude::Implementations]
        );
    }

    #[test]
    fn inactive_object_probe_matches_complete_uris_not_prefixes() {
        let response = r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">
            <adtcore:objectReference adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample2"/>
        </adtcore:objectReferences>"#;

        assert!(
            !inactive_object_response_contains_uri(response, "/sap/bc/adt/oo/classes/zcl_sample")
                .unwrap()
        );
        assert!(
            inactive_object_response_contains_uri(response, "/SAP/BC/ADT/OO/CLASSES/ZCL_SAMPLE2")
                .unwrap()
        );
    }
}
