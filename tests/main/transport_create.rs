//! The HTTP contract for creating a change request.
//!
//! Every part of this was established against a live backend, and each part
//! fails obscurely on its own: without `?_action=INSERT`, or with the dataname
//! the ADT plugin's own classes suggest, SAP answers "Error during
//! deserialization"; asking for the organizer's media type is a 406 naming
//! `text/plain`.
//!
//! The response is the awkward part. This backend answers HTTP 200 with an
//! empty body and no `Location` — the request is created and SAP says nothing
//! about which one it is — so the number is recovered by listing the owner's
//! modifiable requests before and after. These tests pin that, because the
//! failure mode is creating a real transport and reporting failure.

use std::fmt::Write;

use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        transport::{TransportCreateError, TransportTarget, create_transport_request},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

const ORGANIZER_PATH: &str = "/sap/bc/adt/cts/transportrequests";
const CREATE_PATH: &str = "/sap/bc/adt/cts/transports";
const CREATE_MEDIA_TYPE: &str =
    "application/vnd.sap.as+xml; charset=UTF-8; dataname=com.sap.adt.CTS.Request";
const DESCRIPTION: &str = "Widen the event log key";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "100".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        password_command: None,
        edit_packages: None,
        allow_temporary_package: true,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

/// An organizer tree grouping the given requests under target `QA1`.
///
/// `tm:target=""` on the request is what a live backend sends; the target is
/// named only on the group.
fn tree(requests: &[(&str, &str)]) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm" xmlns:adtcore="http://www.sap.com/adt/core">
  <tm:workbench tm:category="Workbench"><tm:target tm:name="QA1"><tm:modifiable tm:status="Modifiable">{}</tm:modifiable></tm:target></tm:workbench>
</tm:root>"#,
        rows(requests)
    )
}

/// The same requests with no enclosing target group: SAP filed them as local.
fn local_tree(requests: &[(&str, &str)]) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm" xmlns:adtcore="http://www.sap.com/adt/core">
  <tm:workbench tm:category="Workbench"><tm:modifiable tm:status="Modifiable">{}</tm:modifiable></tm:workbench>
</tm:root>"#,
        rows(requests)
    )
}

fn rows(requests: &[(&str, &str)]) -> String {
    let mut rows = String::new();
    for (number, description) in requests {
        let _ = write!(
            rows,
            r#"<tm:request tm:number="{number}" tm:owner="DEVELOPER" tm:desc="{description}" tm:type="K" tm:status="D" tm:target=""/>"#
        );
    }
    rows
}

/// Answers the two organizer reads in order: `before`, then `after`.
///
/// Registration order plus `up_to_n_times(1)` is what makes them distinct;
/// the two requests are otherwise identical.
async fn mount_listings(server: &MockServer, before: &str, after: &str) {
    Mock::given(method("GET"))
        .and(path(ORGANIZER_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(before))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(ORGANIZER_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(after))
        .mount(server)
        .await;
}

/// The creation POST, answered the way the live backend answers it.
async fn mount_creation(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(CREATE_PATH))
        // Without this parameter the body is refused as undeserializable.
        .and(query_param("_action", "INSERT"))
        .and(header("content-type", CREATE_MEDIA_TYPE))
        // The organizer's own media type is refused here with a 406.
        .and(header("accept", "text/plain"))
        .and(body_string_contains("<OPERATION>I</OPERATION>"))
        .and(body_string_contains("<DEVCLASS>ZDEMO</DEVCLASS>"))
        .and(body_string_contains(format!(
            "<REQUEST_TEXT>{DESCRIPTION}</REQUEST_TEXT>"
        )))
        .and(body_string_contains("<REQUEST_TYPE>K</REQUEST_TYPE>"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .expect(1)
        .mount(server)
        .await;
}

/// Creating is a POST, so it first fetches a CSRF token through discovery.
async fn client(server: &MockServer) -> SapClient {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "transport-create-token")
                .insert_header("set-cookie", "SAP_SESSIONID=create; Path=/"),
        )
        .mount(server)
        .await;
    SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap()
}

#[tokio::test]
async fn recovers_the_number_of_the_request_that_appeared() {
    let server = MockServer::start().await;
    mount_creation(&server).await;
    mount_listings(
        &server,
        &tree(&[("AB1K900001", "Existing")]),
        &tree(&[("AB1K900001", "Existing"), ("AB1K900005", DESCRIPTION)]),
    )
    .await;

    let created = create_transport_request(
        &mut client(&server).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap();

    assert_eq!(created.number, "AB1K900005");
    assert_eq!(created.description, DESCRIPTION);
    assert_eq!(created.package.as_deref(), Some("ZDEMO"));
    assert_eq!(created.target, TransportTarget::System("QA1".to_owned()));
    server.verify().await;
}

#[tokio::test]
async fn reports_a_request_sap_filed_as_local() {
    // SAP derives the target from the package's transport layer and says
    // nothing when that layer has no route on this system: the request is
    // created, looks ordinary, and can never be released. The absent target is
    // the only signal, so it has to survive to the caller.
    let server = MockServer::start().await;
    mount_creation(&server).await;
    mount_listings(
        &server,
        &local_tree(&[]),
        &local_tree(&[("AB1K900005", DESCRIPTION)]),
    )
    .await;

    let created = create_transport_request(
        &mut client(&server).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap();

    assert_eq!(created.number, "AB1K900005");
    // Seen in the organizer with no target group: local, not merely unseen.
    assert_eq!(created.target, TransportTarget::Local);
    server.verify().await;
}

#[tokio::test]
async fn prefers_a_number_the_backend_reports_over_the_one_that_appeared() {
    // Other releases answer with a CTS object-record URI. The organizer is
    // still read again — the target is never in the response — but the number
    // the backend gave wins over the one the listing implies.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CREATE_PATH))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("/com.sap.cts/object_record/AB1K900007"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_listings(
        &server,
        &tree(&[]),
        // Both are new, and the description points at the *other* one, so
        // taking the number from the response is the only way to get this
        // right.
        &tree(&[
            ("AB1K900007", "Another request"),
            ("AB1K900011", DESCRIPTION),
        ]),
    )
    .await;

    let created = create_transport_request(
        &mut client(&server).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap();

    assert_eq!(created.number, "AB1K900007");
    server.verify().await;
}

#[tokio::test]
async fn a_request_the_organizer_never_showed_has_an_unknown_target_not_a_local_one() {
    // Only reachable on a release that names the request in its response: the
    // number is known without the listing, so the listing may not contain it.
    // Treating that as "no target" claims the request is local and blames the
    // package's transport layer, which is a confident wrong answer about a
    // request nobody has seen.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CREATE_PATH))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("/com.sap.cts/object_record/AB1K900007"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_listings(
        &server,
        &tree(&[]),
        &tree(&[("AB1K900099", "Someone else")]),
    )
    .await;

    let created = create_transport_request(
        &mut client(&server).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap();

    assert_eq!(created.number, "AB1K900007");
    assert_eq!(created.target, TransportTarget::Unknown);
    server.verify().await;
}

#[tokio::test]
async fn tells_two_simultaneous_new_requests_apart_by_description() {
    let server = MockServer::start().await;
    mount_creation(&server).await;
    mount_listings(
        &server,
        &tree(&[]),
        &tree(&[
            ("AB1K900005", "Somebody else's request"),
            ("AB1K900009", DESCRIPTION),
        ]),
    )
    .await;

    let created = create_transport_request(
        &mut client(&server).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap();

    assert_eq!(created.number, "AB1K900009");
    server.verify().await;
}

#[tokio::test]
async fn refuses_to_guess_when_the_new_request_cannot_be_told_apart() {
    // Naming the wrong transport is worse than naming none: the caller would
    // go on to put objects into somebody else's request.
    let server = MockServer::start().await;
    mount_creation(&server).await;
    mount_listings(
        &server,
        &tree(&[]),
        &tree(&[("AB1K900005", DESCRIPTION), ("AB1K900009", DESCRIPTION)]),
    )
    .await;

    let error = create_transport_request(
        &mut client(&server).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "transport_create_number_unknown");
    let hint = error.hint().unwrap();
    // The request exists, so the hint must not read as "nothing happened".
    assert!(hint.contains("do not create another"));
    assert!(hint.contains("AB1K900005") && hint.contains("AB1K900009"));
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal transport list")
    );
    server.verify().await;
}

#[tokio::test]
async fn reports_a_creation_that_left_no_visible_request() {
    let server = MockServer::start().await;
    mount_creation(&server).await;
    mount_listings(&server, &tree(&[]), &tree(&[])).await;

    let error = create_transport_request(
        &mut client(&server).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        TransportCreateError::UnidentifiedRequest { ref candidates } if candidates.is_empty()
    ));
    assert!(error.hint().unwrap().contains("owned by someone else"));
    server.verify().await;
}

#[tokio::test]
async fn the_same_listing_failure_reads_differently_on_each_side_of_the_post() {
    // Creation reads the organizer twice, so one failure means opposite
    // things depending on when it happens. Before the POST nothing exists;
    // after it the request does, and retrying would mint a second one and
    // strand the first.
    let before = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORGANIZER_PATH))
        .respond_with(ResponseTemplate::new(500).set_body_string("organizer unavailable"))
        .mount(&before)
        .await;

    let failed_early = create_transport_request(
        &mut client(&before).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap_err();

    assert_eq!(failed_early.code(), "transport_create_precheck_failed");
    assert!(
        failed_early
            .hint()
            .unwrap()
            .starts_with("No request was created.")
    );
    // Nothing to go looking for, so nothing to suggest.
    assert_eq!(failed_early.suggested_command(), None);
    // The POST is never reached: no POST mock is mounted, and one would 404.
    assert!(
        before
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method != wiremock::http::Method::POST)
    );

    let after = MockServer::start().await;
    mount_creation(&after).await;
    Mock::given(method("GET"))
        .and(path(ORGANIZER_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(tree(&[])))
        .up_to_n_times(1)
        .mount(&after)
        .await;
    Mock::given(method("GET"))
        .and(path(ORGANIZER_PATH))
        .respond_with(ResponseTemplate::new(500).set_body_string("organizer unavailable"))
        .mount(&after)
        .await;

    let failed_late =
        create_transport_request(&mut client(&after).await, DESCRIPTION, "ZDEMO", "DEVELOPER")
            .await
            .unwrap_err();

    assert_eq!(failed_late.code(), "transport_create_unverified");
    let hint = failed_late.hint().unwrap();
    assert!(hint.contains("The request exists"));
    assert!(hint.contains("do not create another"));
    assert_eq!(
        failed_late.suggested_command().as_deref(),
        Some("fractal transport list")
    );
    after.verify().await;
}

#[tokio::test]
async fn a_rejected_creation_says_no_request_was_created() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORGANIZER_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(tree(&[])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CREATE_PATH))
        .respond_with(
            ResponseTemplate::new(403).set_body_string(
                "<error><message>Not authorized to create requests</message></error>",
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = create_transport_request(
        &mut client(&server).await,
        DESCRIPTION,
        "ZDEMO",
        "DEVELOPER",
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "transport_create_failed");
    assert_eq!(error.status(), Some(403));
    assert!(error.hint().unwrap().starts_with("No request was created."));
    server.verify().await;
}

#[tokio::test]
async fn a_blank_description_or_package_never_reaches_sap() {
    let server = MockServer::start().await;

    let blank_description =
        create_transport_request(&mut client(&server).await, "   ", "ZDEMO", "DEVELOPER")
            .await
            .unwrap_err();
    let blank_package =
        create_transport_request(&mut client(&server).await, DESCRIPTION, " ", "DEVELOPER")
            .await
            .unwrap_err();

    assert_eq!(blank_description.code(), "blank_transport_description");
    assert_eq!(blank_package.code(), "blank_transport_package");
    // No mocks are mounted, so any request at all would have been a 404.
    assert!(server.received_requests().await.unwrap().is_empty());
}
