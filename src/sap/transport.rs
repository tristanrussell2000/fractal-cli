//! Change and Transport System (CTS) request management.
//!
//! Endpoints and media types are taken from the backend's own discovery
//! document rather than guessed. Note that listing and creating live under
//! *different* collections: `/cts/transportrequests` is the organizer view,
//! while a new request is posted to `/cts/transports`.

use std::collections::HashSet;

use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use super::{
    client::{SapClient, SapClientError},
    find_attribute_value, find_non_empty_attribute,
};
use crate::{
    reportable_error::{ReportableError, sap_http_status},
    suggested_command,
};

const TRANSPORT_REQUESTS_PATH: &str = "/sap/bc/adt/cts/transportrequests";
/// Creating a request posts to a *different* collection from the one that
/// lists them, per the backend's discovery document.
const TRANSPORTS_PATH: &str = "/sap/bc/adt/cts/transports";
const CREATE_REQUEST_MEDIA_TYPE: &str =
    "application/vnd.sap.as+xml; charset=UTF-8; dataname=com.sap.adt.CTS.Request";
/// The INSERT action answers in plain text, and says so itself: asking for the
/// organizer's own media type is refused with a 406 naming `text/plain`.
const CREATE_RESPONSE_ACCEPT: &str = "text/plain";
/// The organizer returns a bare root element unless this representation is
/// requested explicitly. Note this is *not* the type discovery advertises for
/// the collection (`transportorganizer.v1+xml`, which is the create format);
/// the backend names this one in its 406 response.
const TRANSPORT_ORGANIZER_MEDIA_TYPE: &str =
    "application/vnd.sap.adt.transportorganizertree.v1+xml";
/// One request has its own representation, and it is *not* the one the
/// request's own `adturi` link advertises (`transportrequests.v1+xml`, which is
/// refused with a 406). As with the organizer, the backend's error names the
/// type it will actually accept.
const TRANSPORT_REQUEST_ACCEPT: &str = "application/vnd.sap.adt.transportorganizer.v1+xml";

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

/// A newly created change request, as SAP recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedTransportRequest {
    pub number: String,
    pub description: String,
    pub package: Option<String>,
    /// Where this request will transport to, read back from the organizer.
    ///
    /// SAP derives the target from the package's transport layer and reports
    /// nothing when it cannot, so a package whose layer has no route on this
    /// system produces a local request without any error — it looks fine until
    /// the day it cannot be released.
    pub target: TransportTarget,
}

/// Where a created request will transport to, as SAP recorded it.
///
/// `Local` and `Unknown` are both "no target to report", but they license
/// completely different advice: one is a definite statement about the
/// package's transport layer, the other is an admission that the request was
/// not seen. Collapsing them into `None` produced a confident, wrong diagnosis
/// for a request that simply had not appeared in the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportTarget {
    /// SAP recorded a target system.
    System(String),
    /// SAP recorded no target: the request can hold objects but can never be
    /// released onward.
    Local,
    /// The request did not appear in the organizer, so this is not known.
    Unknown,
}

impl TransportTarget {
    /// The target system, if there is one to name.
    #[must_use]
    pub fn system(&self) -> Option<&str> {
        match self {
            Self::System(name) => Some(name),
            Self::Local | Self::Unknown => None,
        }
    }
}

/// One request in full: its own metadata, its tasks, and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportRequestDetail {
    pub number: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub status: Option<String>,
    /// SAP's own wording for the status, such as `Modifiable`.
    pub status_text: Option<String>,
    pub target: Option<String>,
    /// Every object in the request, from SAP's own aggregate.
    pub objects: Vec<TransportObject>,
    pub tasks: Vec<TransportTaskDetail>,
}

/// A task with the objects recorded against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportTaskDetail {
    pub number: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub status: Option<String>,
    pub status_text: Option<String>,
    /// SAP's task category, such as `Development/Correction`.
    pub task_type: Option<String>,
    pub objects: Vec<TransportObject>,
}

/// One object recorded in a request or task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportObject {
    /// SAP's program ID: `R3TR` for a whole object, `LIMU` for a part of one.
    pub program_id: Option<String>,
    /// The CTS object type, such as `PROG`.
    pub object_type: Option<String>,
    pub name: String,
    /// The workbench type, such as `PROG/P`.
    pub workbench_type: Option<String>,
}

/// A failure while reading one request.
#[derive(Debug, Error)]
pub enum TransportShowError {
    #[error("a request number is required")]
    BlankNumber,
    #[error("transport request {number} does not exist")]
    NotFound { number: String },
    #[error("could not read transport request {number}: {source}")]
    Request {
        number: String,
        #[source]
        source: SapClientError,
    },
    #[error("SAP returned malformed transport XML: {0}")]
    Parse(#[source] roxmltree::Error),
    #[error("SAP's response for {number} contained no request")]
    NoRequest { number: String },
}

impl ReportableError for TransportShowError {
    fn code(&self) -> &'static str {
        match self {
            Self::BlankNumber => "blank_transport_number",
            Self::NotFound { .. } => "transport_not_found",
            Self::Request { .. } => "transport_request_failed",
            Self::Parse(_) | Self::NoRequest { .. } => "transport_response_invalid",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(match self {
            Self::Request { source, .. } => Some(source),
            _ => None,
        })
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::BlankNumber => "Pass the request number, such as ABCK900001.".to_owned(),
            Self::NotFound { .. } => {
                "No request or task with that number exists on this system. Check the number, or list your own requests."
                    .to_owned()
            }
            Self::Request { source, .. } => return source.hint(),
            Self::Parse(_) | Self::NoRequest { .. } => {
                "The SAP transport response did not match the expected ADT XML.".to_owned()
            }
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::BlankNumber | Self::NotFound { .. } => Some(suggested_command::transport_list()),
            _ => None,
        }
    }
}

/// Reads one request, with its tasks and the objects it holds.
///
/// A *task* number is accepted, because SAP accepts it: it answers with the
/// parent request rather than an error, so the number in the result is not
/// necessarily the number that was asked for.
///
/// # Errors
///
/// Returns [`TransportShowError`] for a blank or unknown number, a failed
/// request, or a response that cannot be parsed.
pub async fn show_transport_request(
    sap: &SapClient,
    number: &str,
) -> Result<TransportRequestDetail, TransportShowError> {
    let number = number.trim().to_uppercase();
    if number.is_empty() {
        return Err(TransportShowError::BlankNumber);
    }

    let mut headers = HeaderMap::new();
    headers.insert("Accept", HeaderValue::from_static(TRANSPORT_REQUEST_ACCEPT));
    let response = sap
        .get_bytes_with_query_and_headers(
            &format!("{TRANSPORT_REQUESTS_PATH}/{number}"),
            &[],
            headers,
        )
        .await
        .map_err(|source| {
            if source.is_not_found() {
                TransportShowError::NotFound {
                    number: number.clone(),
                }
            } else {
                TransportShowError::Request {
                    number: number.clone(),
                    source,
                }
            }
        })?;

    parse_transport_request_detail(&String::from_utf8_lossy(&response), &number)
}

fn parse_transport_request_detail(
    xml: &str,
    number: &str,
) -> Result<TransportRequestDetail, TransportShowError> {
    let document = roxmltree::Document::parse(xml).map_err(TransportShowError::Parse)?;
    let request = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "request")
        .ok_or_else(|| TransportShowError::NoRequest {
            number: number.to_owned(),
        })?;
    let tasks = read_tasks(&document);

    Ok(TransportRequestDetail {
        number: find_attribute_value(request, "number")
            .unwrap_or_default()
            .to_owned(),
        description: find_non_empty_attribute(request, "desc"),
        owner: find_non_empty_attribute(request, "owner"),
        status: find_non_empty_attribute(request, "status"),
        status_text: find_non_empty_attribute(request, "status_text"),
        target: find_non_empty_attribute(request, "target"),
        objects: aggregated_objects(request).unwrap_or_else(|| union_of_task_objects(&tasks)),
        tasks,
    })
}

/// Reads the tasks, wherever this response happens to put them.
///
/// Asking for a request nests each `tm:task` inside `tm:request`; asking for a
/// *task* returns the parent's header and puts the task **beside** it, as a
/// sibling under `tm:root`. Searching from the document root covers both, and
/// cannot double-count because tasks do not nest.
fn read_tasks(document: &roxmltree::Document<'_>) -> Vec<TransportTaskDetail> {
    document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "task")
        .map(|task| TransportTaskDetail {
            number: find_attribute_value(task, "number")
                .unwrap_or_default()
                .to_owned(),
            description: find_non_empty_attribute(task, "desc"),
            owner: find_non_empty_attribute(task, "owner"),
            status: find_non_empty_attribute(task, "status"),
            status_text: find_non_empty_attribute(task, "status_text"),
            task_type: find_non_empty_attribute(task, "type"),
            objects: child_elements(task, "abap_object")
                .map(read_object)
                .collect(),
        })
        .collect()
}

/// The request's own object list, which SAP aggregates across its tasks.
///
/// Read from the `tm:all_objects` wrapper's direct children only. Every object
/// appears *twice* when a request is asked for — once here and once under the
/// task that owns it — so a descendant search reports each object twice.
///
/// `None` means the wrapper is absent, which is what asking for a *task*
/// returns: SAP sends the parent's header with no aggregate at all. That is
/// not the same as a request holding nothing.
fn aggregated_objects(request: roxmltree::Node<'_, '_>) -> Option<Vec<TransportObject>> {
    let wrapper = child_elements(request, "all_objects").next()?;
    Some(
        child_elements(wrapper, "abap_object")
            .map(read_object)
            .collect(),
    )
}

/// The objects of every task, for a response that carries no aggregate.
///
/// Deduplicated: an object recorded in two tasks of the same request is one
/// object, and reporting it twice would misstate what the request holds.
fn union_of_task_objects(tasks: &[TransportTaskDetail]) -> Vec<TransportObject> {
    let mut seen = HashSet::new();
    tasks
        .iter()
        .flat_map(|task| task.objects.iter())
        .filter(|object| {
            seen.insert((
                object.program_id.clone(),
                object.object_type.clone(),
                object.name.clone(),
            ))
        })
        .cloned()
        .collect()
}

fn read_object(node: roxmltree::Node<'_, '_>) -> TransportObject {
    TransportObject {
        program_id: find_non_empty_attribute(node, "pgmid"),
        object_type: find_non_empty_attribute(node, "type"),
        name: find_attribute_value(node, "name")
            .unwrap_or_default()
            .to_owned(),
        workbench_type: find_non_empty_attribute(node, "wbtype"),
    }
}

fn child_elements<'a>(
    node: roxmltree::Node<'a, 'a>,
    name: &'static str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'a>> {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

/// A failure while reading the transport organizer.
#[derive(Debug, Error)]
pub enum TransportListError {
    #[error("could not read transport requests: {0}")]
    Request(#[source] SapClientError),
    #[error("SAP returned malformed transport XML: {0}")]
    Parse(#[source] roxmltree::Error),
}

impl ReportableError for TransportListError {
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
        match self {
            Self::Request(error) => error.hint(),
            Self::Parse(_) => Some(
                "The SAP transport organizer response did not match the expected ADT XML."
                    .to_owned(),
            ),
        }
    }
}

/// A failure while creating a change request.
///
/// Creating reads the organizer as well as posting, so the same listing
/// failure means opposite things depending on when it happens: before the
/// POST nothing was created, after it the request exists and must not be
/// created again. Sharing one error with the listing collapsed that
/// distinction and reported a created-but-unnamed request as a read failure,
/// with a hint about connectivity.
#[derive(Debug, Error)]
pub enum TransportCreateError {
    #[error("a new transport request needs a non-blank description")]
    BlankDescription,
    #[error("a new transport request needs a package")]
    BlankPackage,
    /// Failed before anything was created.
    #[error("could not read the existing requests before creating: {0}")]
    Precheck(#[source] TransportListError),
    #[error("the transport-creation request failed: {0}")]
    Request(#[source] SapClientError),
    /// The request was created; the read that would name it failed.
    #[error("the request was created, but the organizer could not be read to name it: {0}")]
    Unverified(#[source] TransportListError),
    /// The request was created; which one it is could not be established.
    #[error(
        "the request was created, but its number could not be determined ({} candidate(s))",
        candidates.len()
    )]
    UnidentifiedRequest { candidates: Vec<String> },
}

impl TransportCreateError {
    /// Whether a request exists despite this failure.
    ///
    /// The two post-creation failures must not read as "nothing happened":
    /// retrying either one mints a second request and leaves the first behind.
    const fn request_exists(&self) -> bool {
        matches!(self, Self::Unverified(_) | Self::UnidentifiedRequest { .. })
    }
}

impl ReportableError for TransportCreateError {
    fn code(&self) -> &'static str {
        match self {
            Self::BlankDescription => "blank_transport_description",
            Self::BlankPackage => "blank_transport_package",
            Self::Precheck(_) => "transport_create_precheck_failed",
            Self::Request(_) => "transport_create_failed",
            Self::Unverified(_) => "transport_create_unverified",
            Self::UnidentifiedRequest { .. } => "transport_create_number_unknown",
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::Precheck(error) | Self::Unverified(error) => error.status(),
            Self::Request(error) => sap_http_status(Some(error)),
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::BlankDescription => {
                "Pass --description with a short summary; SAP stores it as the request text."
                    .to_owned()
            }
            Self::BlankPackage => {
                "Pass --package: SAP uses the package's transport layer to decide the target system, and refuses without one."
                    .to_owned()
            }
            Self::Precheck(error) => return Some(nothing_created(error.hint())),
            Self::Request(error) => return Some(nothing_created(error.hint())),
            Self::Unverified(error) => format!(
                "The request exists — do not create another, or you will leave an orphan behind. List your requests to find it. {}",
                error.hint().unwrap_or_default()
            ),
            Self::UnidentifiedRequest { candidates } => format!(
                "The request exists — do not create another. {} List your requests and use the one you meant.",
                describe_candidates(candidates)
            ),
        })
    }

    fn suggested_command(&self) -> Option<String> {
        self.request_exists()
            .then(suggested_command::transport_list)
    }
}

/// States plainly that nothing exists yet, before whatever advice applies.
fn nothing_created(cause: Option<String>) -> String {
    format!("No request was created. {}", cause.unwrap_or_default())
}

/// What the number-diff saw, so the caller can finish the identification by
/// hand.
fn describe_candidates(candidates: &[String]) -> String {
    match candidates {
        [] => "It did not appear in your modifiable requests, so it may be owned by someone else."
            .to_owned(),
        many => format!(
            "Several appeared at once and none could be told apart by description: {}.",
            many.join(", ")
        ),
    }
}

/// Lists change requests owned by one user.
///
/// # Errors
///
/// Returns [`TransportListError`] when the request fails or its XML cannot be
/// parsed.
pub async fn list_transport_requests(
    sap: &SapClient,
    owner: &str,
    status: TransportStatusFilter,
) -> Result<Vec<TransportRequest>, TransportListError> {
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
        .map_err(TransportListError::Request)?;
    parse_transport_requests(&String::from_utf8_lossy(&response))
}

/// Creates a workbench change request and returns its number.
///
/// The number is not in the response. This backend answers the INSERT action
/// with HTTP 200, an empty body and no `Location`, so the request is created
/// and then identified by listing `owner`'s modifiable requests before and
/// after and taking the one that appeared. The response is still searched
/// first, because other releases do report the number there.
///
/// # Errors
///
/// Returns [`TransportCreateError`] for a blank description, a rejected
/// request, or a creation whose number could not be established afterwards.
/// The variant says whether a request now exists.
pub async fn create_transport_request(
    sap: &mut SapClient,
    description: &str,
    package: &str,
    owner: &str,
) -> Result<CreatedTransportRequest, TransportCreateError> {
    let description = description.trim();
    if description.is_empty() {
        return Err(TransportCreateError::BlankDescription);
    }
    let package = package.trim();
    if package.is_empty() {
        return Err(TransportCreateError::BlankPackage);
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Type",
        HeaderValue::from_static(CREATE_REQUEST_MEDIA_TYPE),
    );
    headers.insert("Accept", HeaderValue::from_static(CREATE_RESPONSE_ACCEPT));

    // Taken before the creation, so the new request can be recognised by not
    // being here.
    let existing = existing_request_numbers(sap, owner)
        .await
        .map_err(TransportCreateError::Precheck)?;

    let (body, location) = sap
        .post_text_with_location(
            TRANSPORTS_PATH,
            &[("_action", "INSERT")],
            Some(&create_request_body(description, package)),
            headers,
        )
        .await
        .map_err(TransportCreateError::Request)?;

    // Report what SAP recorded, not what was asked for. The organizer is read
    // again regardless of whether the response named the request, because the
    // target is the part that decides whether this request is any use, and it
    // is never in the response.
    // Past this point the request exists, so a failure here must not read as
    // "nothing happened".
    let recorded = list_transport_requests(sap, owner, TransportStatusFilter::Modifiable)
        .await
        .map_err(TransportCreateError::Unverified)?;
    let response = format!("{} {body}", location.unwrap_or_default());
    let number = match parse_created_request_number(&response) {
        Some(number) => number,
        None => created_request_number(&recorded, &existing, description)?,
    };
    let target = recorded
        .iter()
        .find(|request| request.number == number)
        .map_or(TransportTarget::Unknown, |request| {
            request
                .target
                .clone()
                .map_or(TransportTarget::Local, TransportTarget::System)
        });

    Ok(CreatedTransportRequest {
        number,
        description: description.to_owned(),
        package: Some(package.to_owned()),
        target,
    })
}

/// SAP's ABAP serialization (`asx`) format.
///
/// The `?_action=INSERT` query parameter, the `com.sap.adt.CTS.Request`
/// dataname, and the `REQUEST_TYPE` field are all required, and omitting any
/// of them fails obscurely: without the action or with a different dataname
/// SAP answers "Error during deserialization", and a body that deserializes
/// into an empty structure is rejected with "Specify a package" even when a
/// package was sent.
///
/// Note that the ADT plugin's own `TransportCreationData` describes a
/// *different* request (`com.sap.adt.CreateCorrectionRequest`, with
/// `REQUEST_PROJECT`/`REQUEST_CHANGE_GUID` fields); that shape is rejected by
/// this endpoint. Prefer confirming against a working client over the
/// plausible-looking one.
fn create_request_body(description: &str, package: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><asx:abap xmlns:asx=\"http://www.sap.com/abapxml\" version=\"1.0\"><asx:values><DATA><OPERATION>I</OPERATION><DEVCLASS>{}</DEVCLASS><REQUEST_TEXT>{}</REQUEST_TEXT><REF></REF><REQUEST_TYPE>K</REQUEST_TYPE></DATA></asx:values></asx:abap>",
        xml_escape(package),
        xml_escape(description)
    )
}

/// The numbers of the requests `owner` can currently change.
async fn existing_request_numbers(
    sap: &SapClient,
    owner: &str,
) -> Result<HashSet<String>, TransportListError> {
    Ok(
        list_transport_requests(sap, owner, TransportStatusFilter::Modifiable)
            .await?
            .into_iter()
            .map(|request| request.number)
            .collect(),
    )
}

/// The number of the request that appeared, for a backend that does not report
/// one.
///
/// Exactly one new request is the ordinary case. If something else created a
/// request at the same moment, the description decides between them; if it
/// still cannot, this fails rather than guessing, because naming the wrong
/// transport is worse than naming none — the caller would go on to put objects
/// into somebody else's request.
fn created_request_number(
    recorded: &[TransportRequest],
    existing: &HashSet<String>,
    description: &str,
) -> Result<String, TransportCreateError> {
    let appeared = recorded
        .iter()
        .filter(|request| !existing.contains(&request.number))
        .collect::<Vec<_>>();

    if appeared.len() > 1 {
        let mut matching = appeared
            .iter()
            .filter(|request| request.description.as_deref() == Some(description));
        if let (Some(request), None) = (matching.next(), matching.next()) {
            return Ok(request.number.clone());
        }
    }
    match appeared.as_slice() {
        [request] => Ok(request.number.clone()),
        candidates => Err(TransportCreateError::UnidentifiedRequest {
            candidates: candidates
                .iter()
                .map(|request| request.number.clone())
                .collect(),
        }),
    }
}

/// The created number, from a response that is not XML.
///
/// The INSERT action answers with a bare CTS object-record URI —
/// `/com.sap.cts/object_record/AB1K900001` — so the number is recognised by
/// its own shape rather than by an element or attribute name. That also makes
/// this tolerant of the organizer-style XML the other CTS endpoints return.
///
/// Failing to parse here is worse than an ordinary parse failure: the request
/// has already been created, so reporting "no number" strands a real transport
/// with nobody holding its identifier.
fn parse_created_request_number(response: &str) -> Option<String> {
    response
        .split(|character: char| !character.is_ascii_alphanumeric())
        .find(|token| is_transport_request_number(token))
        .map(str::to_owned)
}

/// A three-character system ID, `K`, then five digits: `AB1K900001`.
///
/// The system ID is alphanumeric, not alphabetic — a letters-only check here
/// silently matches nothing on any system whose ID contains a digit.
fn is_transport_request_number(token: &str) -> bool {
    let token = token.as_bytes();
    token.len() == 10
        && token[0].is_ascii_uppercase()
        && token[..3]
            .iter()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
        && token[3] == b'K'
        && token[4..].iter().all(u8::is_ascii_digit)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The system a request will transport to, or `None` when it is local.
///
/// SAP sends `tm:target=""` on every request and carries the real value on the
/// `tm:target` node the request is grouped under. Reading the request's own
/// attribute therefore reports *every* request as having no target, which hides
/// the one distinction that matters here: a request with no target can hold
/// objects but can never be released onward.
///
/// The request's own attribute is still preferred when SAP does fill it in,
/// since it is the more specific of the two.
fn request_target(node: roxmltree::Node<'_, '_>) -> Option<String> {
    find_non_empty_attribute(node, "target").or_else(|| {
        node.ancestors()
            .find(|ancestor| ancestor.is_element() && ancestor.tag_name().name() == "target")
            .and_then(|group| find_non_empty_attribute(group, "name"))
    })
}

fn parse_transport_requests(xml: &str) -> Result<Vec<TransportRequest>, TransportListError> {
    let document = roxmltree::Document::parse(xml).map_err(TransportListError::Parse)?;
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
            target: request_target(node),
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

    /// Trimmed to the nesting that matters, but with the attributes exactly as
    /// a live backend sends them: the organizer wraps requests in a category
    /// and a target, tasks are children of their request, and every request
    /// carries `tm:target=""` regardless of where it will transport to.
    const ORGANIZER_TREE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="DEVELOPER">
  <tm:workbench tm:category="Workbench">
    <tm:target tm:name="QA1" tm:desc="Virtual System for Transport Targets">
      <tm:modifiable tm:status="Modifiable">
        <tm:request tm:number="AB1K900001" tm:owner="DEVELOPER" tm:desc="Sample request" tm:type="K" tm:status="D" tm:target="" tm:target_desc="">
          <tm:long_desc/>
          <atom:link xmlns:atom="http://www.w3.org/2005/Atom" href="/sap/bc/adt/cts/transportrequests/AB1K900001" rel="http://www.sap.com/cts/relations/adturi"/>
          <tm:task tm:number="AB1K900002" tm:parent="AB1K900001" tm:owner="DEVELOPER" tm:desc="Sample task" tm:status="D"/>
        </tm:request>
        <tm:request tm:number="AB1K900003" tm:owner="DEVELOPER" tm:desc="" tm:type="K" tm:status="D" tm:target="">
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
        assert_eq!(first.number, "AB1K900001");
        assert_eq!(first.description.as_deref(), Some("Sample request"));
        assert_eq!(first.owner.as_deref(), Some("DEVELOPER"));
        // From the group the request sits in, not from the request itself,
        // which SAP leaves empty.
        assert_eq!(first.target.as_deref(), Some("QA1"));
        assert_eq!(first.tasks.len(), 1);
        assert_eq!(first.tasks[0].number, "AB1K900002");
        assert_eq!(first.tasks[0].description.as_deref(), Some("Sample task"));
    }

    #[test]
    fn treats_the_empty_attributes_sap_always_sends_as_absent() {
        // SAP sends `tm:desc=""` rather than omitting the attribute.
        let requests = parse_transport_requests(ORGANIZER_TREE).unwrap();
        let second = &requests[1];

        assert_eq!(second.number, "AB1K900003");
        assert_eq!(second.description, None);
        assert!(second.tasks.is_empty());
    }

    #[test]
    fn reports_no_target_for_a_request_outside_any_target_group() {
        // A request with no target is local: it can hold objects but can never
        // be released onward, which is the difference this field exists to
        // show. Reading `tm:target` off the request reported *every* request
        // as local, including ones the organizer files under a target.
        let ungrouped = r#"<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm">
  <tm:workbench tm:category="Workbench">
    <tm:request tm:number="AB1K900007" tm:owner="DEVELOPER" tm:desc="Local" tm:type="K" tm:status="D" tm:target=""/>
  </tm:workbench>
</tm:root>"#;

        let requests = parse_transport_requests(ungrouped).unwrap();

        assert_eq!(requests[0].number, "AB1K900007");
        assert_eq!(requests[0].target, None);
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
    fn reads_the_number_out_of_a_cts_object_record_uri() {
        // What a backend that does report the number answers: plain text, not
        // XML, with the number as the last path segment.
        assert_eq!(
            parse_created_request_number("/com.sap.cts/object_record/AB1K900623").as_deref(),
            Some("AB1K900623")
        );
    }

    #[test]
    fn recognises_numbers_from_systems_whose_id_contains_a_digit() {
        // A letters-only system ID check matches nothing on digit-bearing
        // systems, and the failure is silent: the request is created and its
        // number is then reported as unknown.
        assert!(is_transport_request_number("AB1K900623"));
        assert!(is_transport_request_number("ABCK912345"));
        assert!(!is_transport_request_number("AB1K90062"));
        assert!(!is_transport_request_number("AB1X900623"));
        assert!(!is_transport_request_number("ab1k900623"));
        assert!(!is_transport_request_number("AB1K90062X"));
    }

    #[test]
    fn finds_no_number_in_the_empty_response_this_backend_sends() {
        assert_eq!(parse_created_request_number(" "), None);
    }

    #[test]
    fn maps_the_status_filter_to_saps_codes() {
        assert_eq!(TransportStatusFilter::Modifiable.as_sap_code(), "D");
        assert_eq!(TransportStatusFilter::Released.as_sap_code(), "R");
    }
}
