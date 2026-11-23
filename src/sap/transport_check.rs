//! Which change request already holds an object, from ADT's transport check
//! (the call behind Eclipse's transport dialog), used to explain a write SAP
//! refused over its transport.

use std::fmt::Write as _;

use reqwest::header::{HeaderMap, HeaderValue};

use super::{
    client::{SapClient, SapClientError},
    find_child, find_child_text, find_children,
};
use crate::suggested_command;

const TRANSPORT_CHECKS_PATH: &str = "/sap/bc/adt/cts/transportchecks";
const CHECK_DATA_MEDIA_TYPE: &str =
    "application/vnd.sap.as+xml; charset=UTF-8; dataname=com.sap.adt.transport.service.checkData";

/// An open request holding the object, with the tasks its entries are in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockHolder {
    pub request: String,
    pub owner: Option<String>,
    pub description: Option<String>,
    pub tasks: Vec<String>,
}

/// What the transport check says about one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportCheck {
    /// Whether changes to the object are recorded in a request at all.
    pub recording: bool,
    pub holders: Vec<LockHolder>,
}

/// A write SAP refused over its transport, with the requests holding the object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportRefusal {
    named: Option<String>,
    caller: String,
    check: TransportCheck,
}

/// Whether SAP refused a write because of the transport it named, or lacked.
#[must_use]
pub fn is_transport_refusal(error: &SapClientError) -> bool {
    let SapClientError::Http { message, .. } = error else {
        return false;
    };
    let message = message.to_ascii_lowercase();
    message.contains("corrnr")
        || (message.contains("locked") && (message.contains("request") || message.contains("task")))
}

/// Asks SAP which requests hold the object at `uri`. Read-only.
///
/// # Errors
///
/// Returns [`SapClientError`] when the request fails.
pub async fn check_transport(
    sap: &mut SapClient,
    uri: &str,
    mut headers: HeaderMap,
) -> Result<TransportCheck, SapClientError> {
    headers.insert(
        "Content-Type",
        HeaderValue::from_static(CHECK_DATA_MEDIA_TYPE),
    );
    headers.insert("Accept", HeaderValue::from_static(CHECK_DATA_MEDIA_TYPE));
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><asx:abap version="1.0" xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA><PGMID/><OBJECT/><OBJECTNAME/><DEVCLASS/><SUPER_PACKAGE/><OPERATION>I</OPERATION><URI>{}</URI></DATA></asx:values></asx:abap>"#,
        escape(uri)
    );
    let response = sap
        .post_text(TRANSPORT_CHECKS_PATH, &[], Some(&body), headers)
        .await?;
    Ok(parse_transport_check(&response))
}

/// Explains a refused write, when the refusal is about its transport and the
/// check answers. Any failure of the check itself leaves the refusal as SAP
/// put it.
pub async fn explain_refusal(
    sap: &mut SapClient,
    uri: &str,
    named: Option<&str>,
    error: &SapClientError,
    headers: HeaderMap,
) -> Option<Box<TransportRefusal>> {
    if !is_transport_refusal(error) {
        return None;
    }
    let check = check_transport(sap, uri, headers).await.ok()?;
    Some(Box::new(TransportRefusal {
        named: named.map(str::to_ascii_uppercase),
        caller: sap.username().to_ascii_uppercase(),
        check,
    }))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn parse_transport_check(xml: &str) -> TransportCheck {
    let Ok(document) = roxmltree::Document::parse(xml) else {
        return TransportCheck {
            recording: false,
            holders: Vec::new(),
        };
    };
    let data = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "DATA");
    let recording = data
        .and_then(|data| find_child_text(data, "RECORDING"))
        .is_some_and(|flag| flag == "X");
    let mut holders: Vec<LockHolder> = Vec::new();
    for holder in data
        .and_then(|data| find_child(data, "LOCKS"))
        .into_iter()
        .flat_map(|locks| find_children(locks, "CTS_OBJECT_LOCK"))
        .filter_map(|lock| find_child(lock, "LOCK_HOLDER"))
    {
        let Some(header) = find_child(holder, "REQ_HEADER") else {
            continue;
        };
        let Some(request) = find_child_text(header, "TRKORR") else {
            continue;
        };
        let tasks = find_child(holder, "TASK_HEADERS")
            .into_iter()
            .flat_map(|tasks| find_children(tasks, "CTS_TASK_HEADER"))
            .filter_map(|task| find_child_text(task, "TRKORR"));
        if let Some(known) = holders.iter_mut().find(|known| known.request == request) {
            for task in tasks {
                if !known.tasks.contains(&task) {
                    known.tasks.push(task);
                }
            }
        } else {
            holders.push(LockHolder {
                request,
                owner: find_child_text(header, "AS4USER"),
                description: find_child_text(header, "AS4TEXT"),
                tasks: tasks.collect(),
            });
        }
    }
    TransportCheck { recording, holders }
}

impl LockHolder {
    fn describe(&self) -> String {
        let mut text = self.request.clone();
        let details: Vec<String> = [
            self.owner.clone(),
            self.description.as_ref().map(|text| format!("\"{text}\"")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !details.is_empty() {
            let _ = write!(text, " ({})", details.join(", "));
        }
        text
    }
}

impl TransportRefusal {
    /// What to do about the refusal, or `None` when the check adds nothing.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match self.check.holders.as_slice() {
            [] => (self.named.is_none() && self.check.recording).then(|| {
                "SAP needs a transport for this object, and no open request holds it yet. Pass --transport with one of your requests (`fractal transport list`), or create one with `fractal transport create`."
                    .to_owned()
            }),
            [holder] => Some(self.single_holder_hint(holder)),
            holders => Some(format!(
                "Parts of the object are in several open requests: {}. Pass --transport with the one holding the part being changed.",
                holders
                    .iter()
                    .map(LockHolder::describe)
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
        }
    }

    fn single_holder_hint(&self, holder: &LockHolder) -> String {
        let request = &holder.request;
        let mut hint = match self.named.as_deref() {
            None => format!(
                "The object is already in request {}. Retry with --transport {request}.",
                holder.describe()
            ),
            Some(named) if named == request => {
                return format!(
                    "SAP refused {}, the request already holding the object. Check that your user may write to it.",
                    holder.describe()
                );
            }
            Some(named) if holder.tasks.iter().any(|task| task == named) => format!(
                "{named} is a task of request {}; name the request instead: --transport {request}.",
                holder.describe()
            ),
            Some(named) => format!(
                "The object is already in request {}, so it cannot be recorded in {named} until that request is released. Retry with --transport {request}.",
                holder.describe()
            ),
        };
        if holder
            .owner
            .as_deref()
            .is_some_and(|owner| !owner.eq_ignore_ascii_case(&self.caller))
        {
            hint.push_str(
                " That request is someone else's: naming it records your change there, under a task SAP adds for you.",
            );
        }
        hint
    }

    /// `transport show` for the one request holding the object.
    #[must_use]
    pub fn suggested_command(&self) -> Option<String> {
        match self.check.holders.as_slice() {
            [holder] => Some(suggested_command::transport_show(&holder.request)),
            [] if self.check.recording => Some(suggested_command::transport_list()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/testdata/transport_check/{name}.xml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn refusal(fixture_name: &str, named: Option<&str>, caller: &str) -> TransportRefusal {
        TransportRefusal {
            named: named.map(str::to_owned),
            caller: caller.to_owned(),
            check: parse_transport_check(&fixture(fixture_name)),
        }
    }

    #[test]
    fn reads_the_request_holding_the_object_with_its_tasks() {
        let check = parse_transport_check(&fixture("held"));
        assert!(check.recording);
        assert_eq!(
            check.holders,
            vec![LockHolder {
                request: "A4HK900781".to_owned(),
                owner: Some("DEVUSER2".to_owned()),
                description: Some("Sample fixes".to_owned()),
                tasks: vec!["A4HK900783".to_owned()],
            }]
        );
    }

    #[test]
    fn an_unrecorded_object_and_a_local_one_have_no_holder() {
        let unrecorded = parse_transport_check(&fixture("unrecorded"));
        assert!(unrecorded.recording);
        assert!(unrecorded.holders.is_empty());
        let local = parse_transport_check(&fixture("local"));
        assert!(!local.recording);
        assert!(local.holders.is_empty());
    }

    #[test]
    fn no_transport_names_the_holding_request() {
        let refusal = refusal("held", None, "DEVUSER2");
        assert_eq!(
            refusal.hint().unwrap(),
            "The object is already in request A4HK900781 (DEVUSER2, \"Sample fixes\"). Retry with --transport A4HK900781."
        );
        assert_eq!(
            refusal.suggested_command().unwrap(),
            "fractal transport show A4HK900781"
        );
    }

    #[test]
    fn a_task_is_answered_with_its_request() {
        let hint = refusal("held", Some("A4HK900783"), "DEVUSER2")
            .hint()
            .unwrap();
        assert!(hint.starts_with("A4HK900783 is a task of request A4HK900781"));
        assert!(hint.contains("--transport A4HK900781"));
    }

    #[test]
    fn another_users_request_is_called_out() {
        let hint = refusal("held", Some("A4HK900801"), "DEVUSER1")
            .hint()
            .unwrap();
        assert!(hint.contains("cannot be recorded in A4HK900801"));
        assert!(hint.contains("someone else's"));
    }

    #[test]
    fn an_unrecorded_object_without_a_transport_is_pointed_at_the_requests() {
        let unrecorded = refusal("unrecorded", None, "DEVUSER1");
        assert!(
            unrecorded
                .hint()
                .unwrap()
                .contains("no open request holds it")
        );
        assert_eq!(
            unrecorded.suggested_command().unwrap(),
            "fractal transport list"
        );
        assert_eq!(refusal("local", None, "DEVUSER1").hint(), None);
    }

    #[test]
    fn only_transport_refusals_are_explained() {
        let http = |message: &str| SapClientError::Http {
            kind: crate::sap::client::SapHttpErrorKind::Other,
            status: reqwest::StatusCode::BAD_REQUEST,
            url: "https://sap.example".to_owned(),
            message: message.to_owned(),
        };
        assert!(is_transport_refusal(&http(
            "Parameter corrNr could not be found."
        )));
        assert!(is_transport_refusal(&http(
            "Object R3TR PROG ZSAMPLE is already locked in request A4HK900789 of user DEVUSER1"
        )));
        assert!(!is_transport_refusal(&http("Syntax error in line 3")));
    }
}
