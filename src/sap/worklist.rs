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
    find_attribute_value, find_child, find_non_empty_attribute,
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

/// One object on the caller's worklist. A class's staged includes and
/// sections are folded into the class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InactiveObject {
    /// The type before the first `/` of ADT's type: `CLAS` for `CLAS/OC`.
    pub object_type: String,
    pub adt_type: String,
    pub name: String,
    pub uri: String,
    /// For a class, which of its source streams are staged.
    pub parts: Vec<ClassInclude>,
    /// Staged for deletion rather than changed.
    pub deleted: bool,
    pub transport: Option<InactiveTransport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InactiveTransport {
    pub request: String,
    /// The task under the request that holds the object, when SAP names one.
    pub task: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Error)]
pub enum InactiveObjectsError {
    #[error("could not read the inactive objects: {0}")]
    Sap(#[from] SapClientError),
    #[error("SAP returned malformed inactive-object XML: {0}")]
    Parse(#[from] roxmltree::Error),
}

impl ReportableError for InactiveObjectsError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(_) => "inactive_objects_response_invalid",
        }
    }

    fn status(&self) -> Option<u16> {
        crate::reportable_error::sap_http_status(match self {
            Self::Sap(error) => Some(error),
            Self::Parse(_) => None,
        })
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(_) => {
                Some("The SAP inactive-object response did not contain valid ADT XML.".to_owned())
            }
        }
    }
}

/// What the caller has staged and not activated, one row per object.
///
/// # Errors
///
/// Returns [`InactiveObjectsError`] when the worklist cannot be read or parsed.
pub async fn list_inactive_objects(
    sap: &SapClient,
) -> Result<Vec<InactiveObject>, InactiveObjectsError> {
    parse_inactive_objects(&sap.get_text(INACTIVE_OBJECTS_PATH).await?)
}

fn parse_inactive_objects(response: &str) -> Result<Vec<InactiveObject>, InactiveObjectsError> {
    let document = roxmltree::Document::parse(response)?;
    let mut objects: Vec<InactiveObject> = Vec::new();
    for entry in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "entry")
    {
        // An entry without an object heads a transport group. SAP does not
        // list the group's objects after it, so each object's own transport
        // is what places it.
        let Some(object) = find_child(entry, "object") else {
            continue;
        };
        let Some(reference) = find_child(object, "ref") else {
            continue;
        };
        let Some(uri) = find_non_empty_attribute(reference, "uri") else {
            continue;
        };
        let adt_type = find_non_empty_attribute(reference, "type").unwrap_or_default();
        let name = find_non_empty_attribute(reference, "name").unwrap_or_default();
        let deleted = find_attribute_value(object, "deleted") == Some("true");
        let transport = find_child(entry, "transport")
            .and_then(|transport| find_child(transport, "ref"))
            .and_then(transport_of);

        let (object_uri, part) = match ClassInclude::split_source_uri(&uri) {
            Some((class_uri, include)) => (class_uri.to_owned(), Some(include)),
            None => (uri.clone(), None),
        };
        let index = match objects
            .iter()
            .position(|known| known.uri.eq_ignore_ascii_case(&object_uri))
        {
            Some(index) => index,
            None => {
                let object_type = adt_type.split('/').next().unwrap_or_default().to_owned();
                objects.push(InactiveObject {
                    object_type,
                    adt_type: if part.is_some() {
                        String::new()
                    } else {
                        adt_type.clone()
                    },
                    name,
                    uri: object_uri,
                    parts: Vec::new(),
                    deleted,
                    transport: None,
                });
                objects.len() - 1
            }
        };
        let known = &mut objects[index];
        if let Some(part) = part {
            known.parts.push(part);
        } else {
            known.adt_type = adt_type;
            known.deleted = deleted;
        }
        if known.transport.is_none() {
            known.transport = transport;
        }
    }
    for object in &mut objects {
        object.parts.sort_unstable();
        object.parts.dedup();
    }
    objects.sort_by(|left, right| {
        (&left.object_type, &left.name).cmp(&(&right.object_type, &right.name))
    });
    Ok(objects)
}

fn transport_of(reference: roxmltree::Node<'_, '_>) -> Option<InactiveTransport> {
    let name = find_non_empty_attribute(reference, "name")?;
    let description = find_non_empty_attribute(reference, "description");
    Some(match find_non_empty_attribute(reference, "parentUri") {
        Some(parent) => InactiveTransport {
            request: parent
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase(),
            task: Some(name),
            description,
        },
        None => InactiveTransport {
            request: name,
            task: None,
            description,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live answer, renamed: a new class in `$TMP` and a new program on a
    /// task of a request. The program is listed after the "no transport"
    /// header although it belongs to the request.
    const LIVE: &str = r#"<?xml version="1.0" encoding="utf-8"?><ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"><ioc:entry><ioc:object/><ioc:transport ioc:user="DEVELOPER" ioc:linked="false"><ioc:ref adtcore:uri="/sap/bc/adt/cts/transportrequests/A4HK900123" adtcore:type="/RQ" adtcore:name="A4HK900123" adtcore:description="Sample request" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:transport></ioc:entry><ioc:entry><ioc:object/><ioc:transport/></ioc:entry><ioc:entry><ioc:object ioc:user="DEVELOPER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample" adtcore:type="CLAS/OC" adtcore:name="ZCL_SAMPLE" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport/></ioc:entry><ioc:entry><ioc:object ioc:user="DEVELOPER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/includes/definitions" adtcore:type="CLAS/OCN/definitions" adtcore:name="ZCL_SAMPLE" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport/></ioc:entry><ioc:entry><ioc:object ioc:user="DEVELOPER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/includes/implementations" adtcore:type="CLAS/OCN/implementations" adtcore:name="ZCL_SAMPLE" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport/></ioc:entry><ioc:entry><ioc:object ioc:user="DEVELOPER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/includes/macros" adtcore:type="CLAS/OCN/macros" adtcore:name="ZCL_SAMPLE" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport/></ioc:entry><ioc:entry><ioc:object ioc:user="DEVELOPER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/source/main#type=CLAS%2FOSI;name=ZCL_SAMPLE" adtcore:type="CLAS/OSI" adtcore:name="ZCL_SAMPLE" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport/></ioc:entry><ioc:entry><ioc:object ioc:user="DEVELOPER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/source/main#type=CLAS%2FOSO;name=ZCL_SAMPLE" adtcore:type="CLAS/OSO" adtcore:name="ZCL_SAMPLE" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport/></ioc:entry><ioc:entry><ioc:object ioc:user="DEVELOPER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/source/main#type=CLAS%2FOSU;name=ZCL_SAMPLE" adtcore:type="CLAS/OSU" adtcore:name="ZCL_SAMPLE" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport/></ioc:entry><ioc:entry><ioc:object ioc:user="DEVELOPER" ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/programs/programs/zsample_prog" adtcore:type="PROG/P" adtcore:name="ZSAMPLE_PROG" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:object><ioc:transport ioc:user="DEVELOPER" ioc:linked="true"><ioc:ref adtcore:uri="/sap/bc/adt/cts/transportrequests/A4HK900124" adtcore:type="/RQ" adtcore:name="A4HK900124" adtcore:parentUri="/sap/bc/adt/cts/transportrequests/A4HK900123" adtcore:description="Sample request" xmlns:adtcore="http://www.sap.com/adt/core"/></ioc:transport></ioc:entry></ioc:inactiveObjects>"#;

    #[test]
    fn lists_one_row_per_object_with_its_own_transport() {
        let objects = parse_inactive_objects(LIVE).unwrap();
        assert_eq!(
            objects,
            vec![
                InactiveObject {
                    object_type: "CLAS".to_owned(),
                    adt_type: "CLAS/OC".to_owned(),
                    name: "ZCL_SAMPLE".to_owned(),
                    uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                    parts: vec![
                        ClassInclude::Main,
                        ClassInclude::Definitions,
                        ClassInclude::Implementations,
                        ClassInclude::Macros,
                    ],
                    deleted: false,
                    transport: None,
                },
                InactiveObject {
                    object_type: "PROG".to_owned(),
                    adt_type: "PROG/P".to_owned(),
                    name: "ZSAMPLE_PROG".to_owned(),
                    uri: "/sap/bc/adt/programs/programs/zsample_prog".to_owned(),
                    parts: Vec::new(),
                    deleted: false,
                    transport: Some(InactiveTransport {
                        request: "A4HK900123".to_owned(),
                        task: Some("A4HK900124".to_owned()),
                        description: Some("Sample request".to_owned()),
                    }),
                },
            ]
        );
    }

    #[test]
    fn a_class_with_only_an_include_staged_is_still_one_class() {
        let response = r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects" xmlns:adtcore="http://www.sap.com/adt/core">
            <ioc:entry><ioc:object ioc:deleted="false"><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/includes/implementations" adtcore:type="CLAS/OCN/implementations" adtcore:name="ZCL_SAMPLE" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample"/></ioc:object><ioc:transport/></ioc:entry>
            <ioc:entry><ioc:object ioc:deleted="true"><ioc:ref adtcore:uri="/sap/bc/adt/ddic/ddl/sources/zsample_view" adtcore:type="DDLS/DF" adtcore:name="ZSAMPLE_VIEW"/></ioc:object><ioc:transport/></ioc:entry>
        </ioc:inactiveObjects>"#;
        let objects = parse_inactive_objects(response).unwrap();
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].object_type, "CLAS");
        assert_eq!(objects[0].name, "ZCL_SAMPLE");
        assert_eq!(objects[0].uri, "/sap/bc/adt/oo/classes/zcl_sample");
        assert_eq!(objects[0].parts, vec![ClassInclude::Implementations]);
        assert_eq!(objects[1].object_type, "DDLS");
        assert!(objects[1].deleted);
    }

    #[test]
    fn an_empty_worklist_is_an_empty_list() {
        let response = r#"<?xml version="1.0" encoding="utf-8"?><ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"/>"#;
        assert!(parse_inactive_objects(response).unwrap().is_empty());
    }

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
