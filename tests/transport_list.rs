//! The transport organizer's HTTP contract.
//!
//! The central assertions are the `Accept` header and the four query
//! parameters. Both were established against a live backend and both fail
//! quietly if wrong: the wrong media type is a 406, but the wrong parameters
//! return a valid, *empty* `tm:root` — a successful response reporting no
//! requests, which is indistinguishable from a user who genuinely has none.

use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        transport::{TransportListError, TransportStatusFilter, list_transport_requests},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

const ORGANIZER_PATH: &str = "/sap/bc/adt/cts/transportrequests";
const ORGANIZER_MEDIA_TYPE: &str = "application/vnd.sap.adt.transportorganizertree.v1+xml";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "100".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        password_command: None,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

fn organizer_tree() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="DEVELOPER">
  <tm:workbench tm:category="Workbench">
    <tm:target tm:name="QA1" tm:desc="Virtual System">
      <tm:modifiable tm:status="Modifiable">
        <tm:request tm:number="AB1K900001" tm:owner="DEVELOPER" tm:desc="First request" tm:type="K" tm:status="D" tm:target="">
          <tm:task tm:number="AB1K900002" tm:parent="AB1K900001" tm:owner="DEVELOPER" tm:desc="First task" tm:status="D"/>
        </tm:request>
        <tm:request tm:number="AB1K900003" tm:owner="DEVELOPER" tm:desc="Second request" tm:type="K" tm:status="D" tm:target="">
          <tm:task tm:number="AB1K900004" tm:parent="AB1K900003" tm:owner="DEVELOPER" tm:desc="Second task" tm:status="D"/>
        </tm:request>
      </tm:modifiable>
    </tm:target>
  </tm:workbench>
</tm:root>"#
}

async fn mount_organizer(server: &MockServer, status_code: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(ORGANIZER_PATH))
        .and(query_param("sap-client", "100"))
        .and(query_param("user", "DEVELOPER"))
        // Without every one of these the backend answers 200 with an empty
        // tree, so each is pinned individually.
        .and(query_param("targets", "true"))
        .and(query_param("requestStatus", status_code))
        .and(query_param("requestType", "K"))
        .and(header("accept", ORGANIZER_MEDIA_TYPE))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn requests_the_organizer_tree_and_reads_requests_with_their_tasks() {
    let server = MockServer::start().await;
    mount_organizer(
        &server,
        "D",
        ResponseTemplate::new(200).set_body_string(organizer_tree()),
    )
    .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let requests = list_transport_requests(&client, "DEVELOPER", TransportStatusFilter::Modifiable)
        .await
        .unwrap();

    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].number, "AB1K900001");
    assert_eq!(requests[0].description.as_deref(), Some("First request"));
    // SAP sends tm:target="" on the request and names the target only on the
    // group it sits under, so reading the request's own attribute reports every
    // request as local.
    assert_eq!(requests[0].target.as_deref(), Some("QA1"));

    // Tasks must attach to their own request. Reading them as descendants of
    // the whole tree would give both requests both tasks.
    assert_eq!(requests[0].tasks.len(), 1);
    assert_eq!(requests[0].tasks[0].number, "AB1K900002");
    assert_eq!(requests[1].tasks.len(), 1);
    assert_eq!(requests[1].tasks[0].number, "AB1K900004");
    server.verify().await;
}

#[tokio::test]
async fn listing_released_requests_changes_only_the_status_parameter() {
    let server = MockServer::start().await;
    mount_organizer(
        &server,
        "R",
        ResponseTemplate::new(200).set_body_string(organizer_tree()),
    )
    .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    list_transport_requests(&client, "DEVELOPER", TransportStatusFilter::Released)
        .await
        .unwrap();

    server.verify().await;
}

#[tokio::test]
async fn an_empty_tree_is_reported_as_no_requests_rather_than_an_error() {
    let server = MockServer::start().await;
    mount_organizer(
        &server,
        "D",
        ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?><tm:root xmlns:tm="http://www.sap.com/cts/adt/tm"/>"#,
        ),
    )
    .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let requests = list_transport_requests(&client, "DEVELOPER", TransportStatusFilter::Modifiable)
        .await
        .unwrap();

    assert!(requests.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn a_refused_media_type_surfaces_saps_own_message() {
    // What the backend actually answers when the Accept header is wrong; the
    // message names the type it wants, which is how the right one was found.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ORGANIZER_PATH))
        .respond_with(ResponseTemplate::new(406).set_body_string(
            "<error><message>The message content is not acceptable. Accepted content types: application/vnd.sap.adt.transportorganizertree.v1+xml</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = list_transport_requests(&client, "DEVELOPER", TransportStatusFilter::Modifiable)
        .await
        .unwrap_err();

    assert!(matches!(error, TransportListError::Request(_)));
    assert_eq!(error.code(), "transport_request_failed");
    assert_eq!(error.status(), Some(406));
    assert!(error.message().contains("transportorganizertree"));
    server.verify().await;
}

#[tokio::test]
async fn malformed_organizer_xml_is_a_stable_parse_error() {
    let server = MockServer::start().await;
    mount_organizer(
        &server,
        "D",
        ResponseTemplate::new(200).set_body_string("<tm:root"),
    )
    .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = list_transport_requests(&client, "DEVELOPER", TransportStatusFilter::Modifiable)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "transport_response_invalid");
    assert_eq!(error.status(), None);
    assert!(error.hint().is_some());
    server.verify().await;
}
