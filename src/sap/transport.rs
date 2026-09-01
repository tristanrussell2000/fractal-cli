//! Change and Transport System (CTS) request management.
//!
//! Endpoints and media types are taken from the backend's own discovery
//! document rather than guessed. Note that listing and creating live under
//! *different* collections: `/cts/transportrequests` is the organizer view,
//! while a new request is posted to `/cts/transports`.

use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use super::{
    client::{SapClient, SapClientError},
    find_attribute_value, find_non_empty_attribute,
};
use crate::reportable_error::{ReportableError, sap_http_status};

const TRANSPORT_REQUESTS_PATH: &str = "/sap/bc/adt/cts/transportrequests";
/// The organizer returns a bare root element unless this representation is
/// requested explicitly. Note this is *not* the type discovery advertises for
/// the collection (`transportorganizer.v1+xml`, which is the create format);
/// the backend names this one in its 406 response.
const TRANSPORT_ORGANIZER_MEDIA_TYPE: &str =
    "application/vnd.sap.adt.transportorganizertree.v1+xml";

/// One change request, with the tasks it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportRequest {
    pub number: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub status: Option<String>,
    pub target: Option<String>,
    pub tasks: Vec<TransportTask>,
}

/// A task belonging to a request. Objects are recorded against tasks, and a
/// task is what a developer is usually assigned, but only the parent request
/// can be released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportTask {
    pub number: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub status: Option<String>,
}

/// Which requests to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportStatusFilter {
    /// Requests that can still be changed.
    Modifiable,
    /// Requests already released to the next system.
    Released,
}

impl TransportStatusFilter {
    /// SAP's single-letter `trstatus` code.
    const fn as_sap_code(self) -> &'static str {
        match self {
            Self::Modifiable => "D",
            Self::Released => "R",
        }
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("could not read transport requests: {0}")]
    Request(#[source] SapClientError),
    #[error("SAP returned malformed transport XML: {0}")]
    Parse(#[source] roxmltree::Error),
}

impl ReportableError for TransportError {
    fn code(&self) -> &'static str {
        match self {
            Self::Request(_) => "transport_request_failed",
            Self::Parse(_) => "transport_response_invalid",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(match self {
            Self::Request(error) => Some(error),
            Self::Parse(_) => None,
        })
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Request(error) => return error.hint(),
            Self::Parse(_) => {
                "The SAP transport organizer response did not match the expected ADT XML."
                    .to_owned()
            }
        })
    }
}

/// Lists change requests owned by one user.
///
/// # Errors
///
/// Returns [`TransportError`] when the request fails or its XML cannot be
/// parsed.
pub async fn list_transport_requests(
    sap: &SapClient,
    owner: &str,
    status: TransportStatusFilter,
) -> Result<Vec<TransportRequest>, TransportError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Accept",
        HeaderValue::from_static(TRANSPORT_ORGANIZER_MEDIA_TYPE),
    );
    let response = sap
        .get_bytes_with_query_and_headers(
            TRANSPORT_REQUESTS_PATH,
            &[
                ("user", owner),
                ("targets", "true"),
                ("requestStatus", status.as_sap_code()),
                // Workbench requests only: customizing requests do not carry
                // the repository objects this CLI edits.
                ("requestType", "K"),
            ],
            headers,
        )
        .await
        .map_err(TransportError::Request)?;
    parse_transport_requests(&String::from_utf8_lossy(&response))
}

fn parse_transport_requests(xml: &str) -> Result<Vec<TransportRequest>, TransportError> {
    let document = roxmltree::Document::parse(xml).map_err(TransportError::Parse)?;
    Ok(document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "request")
        .map(|node| TransportRequest {
            number: find_attribute_value(node, "number")
                .unwrap_or_default()
                .to_owned(),
            description: find_non_empty_attribute(node, "desc"),
            owner: find_non_empty_attribute(node, "owner"),
            status: find_non_empty_attribute(node, "status"),
            target: find_non_empty_attribute(node, "target"),
            tasks: node
                .children()
                .filter(|task| task.is_element() && task.tag_name().name() == "task")
                .map(|task| TransportTask {
                    number: find_attribute_value(task, "number")
                        .unwrap_or_default()
                        .to_owned(),
                    description: find_non_empty_attribute(task, "desc"),
                    owner: find_non_empty_attribute(task, "owner"),
                    status: find_non_empty_attribute(task, "status"),
                })
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed to the nesting that matters: the organizer wraps requests in a
    /// category and a target, and tasks are children of their request.
    const ORGANIZER_TREE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="DEVELOPER">
  <tm:workbench tm:category="Workbench">
    <tm:target tm:name="XE1" tm:desc="Virtual System">
      <tm:modifiable tm:status="Modifiable">
        <tm:request tm:number="DE3K900001" tm:owner="DEVELOPER" tm:desc="Sample request" tm:type="K" tm:status="D" tm:target="XE1">
          <tm:long_desc/>
          <atom:link xmlns:atom="http://www.w3.org/2005/Atom" href="/sap/bc/adt/cts/transportrequests/DE3K900001" rel="http://www.sap.com/cts/relations/adturi"/>
          <tm:task tm:number="DE3K900002" tm:parent="DE3K900001" tm:owner="DEVELOPER" tm:desc="Sample task" tm:status="D"/>
        </tm:request>
        <tm:request tm:number="DE3K900003" tm:owner="DEVELOPER" tm:desc="" tm:type="K" tm:status="D" tm:target="">
        </tm:request>
      </tm:modifiable>
    </tm:target>
  </tm:workbench>
</tm:root>"#;

    #[test]
    fn reads_requests_and_their_tasks_out_of_the_organizer_tree() {
        let requests = parse_transport_requests(ORGANIZER_TREE).unwrap();

        assert_eq!(requests.len(), 2);
        let first = &requests[0];
        assert_eq!(first.number, "DE3K900001");
        assert_eq!(first.description.as_deref(), Some("Sample request"));
        assert_eq!(first.owner.as_deref(), Some("DEVELOPER"));
        assert_eq!(first.target.as_deref(), Some("XE1"));
        assert_eq!(first.tasks.len(), 1);
        assert_eq!(first.tasks[0].number, "DE3K900002");
        assert_eq!(first.tasks[0].description.as_deref(), Some("Sample task"));
    }

    #[test]
    fn treats_the_empty_attributes_sap_always_sends_as_absent() {
        // SAP sends `tm:desc=""` and `tm:target=""` rather than omitting them.
        let requests = parse_transport_requests(ORGANIZER_TREE).unwrap();
        let second = &requests[1];

        assert_eq!(second.number, "DE3K900003");
        assert_eq!(second.description, None);
        assert_eq!(second.target, None);
        assert!(second.tasks.is_empty());
    }

    #[test]
    fn an_empty_tree_is_not_an_error() {
        let empty = r#"<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm"/>"#;

        assert!(parse_transport_requests(empty).unwrap().is_empty());
    }

    #[test]
    fn rejects_malformed_transport_xml() {
        let error = parse_transport_requests("<not-closed").unwrap_err();

        assert_eq!(error.code(), "transport_response_invalid");
        assert!(error.hint().is_some());
    }

    #[test]
    fn maps_the_status_filter_to_saps_codes() {
        assert_eq!(TransportStatusFilter::Modifiable.as_sap_code(), "D");
        assert_eq!(TransportStatusFilter::Released.as_sap_code(), "R");
    }
}
