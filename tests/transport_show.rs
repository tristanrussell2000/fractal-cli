//! Reading one request, its tasks, and the objects it holds.
//!
//! The endpoint answers in two different shapes depending on what is asked
//! for, and both are pinned here because each mis-parses in a way that looks
//! like a plausible answer rather than an error:
//!
//! - a **request** nests its tasks inside `tm:request` and repeats every object
//!   in a `tm:all_objects` aggregate, so a descendant search doubles the count;
//! - a **task** returns the *parent's* header with the task as a **sibling** of
//!   it and no aggregate at all, so reading only nested tasks reports a request
//!   that holds nothing.
//!
//! The media type is the third trap: the request's own `adturi` link advertises
//! `transportrequests.v1+xml`, which the endpoint refuses with a 406 naming
//! `transportorganizer.v1+xml` instead.

use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        transport::{TransportShowError, show_transport_request},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

const REQUEST_PATH: &str = "/sap/bc/adt/cts/transportrequests/AB1K900001";
const REQUEST_MEDIA_TYPE: &str = "application/vnd.sap.adt.transportorganizer.v1+xml";

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

fn client(server: &MockServer) -> SapClient {
    SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap()
}

async fn mount(server: &MockServer, body: &str) {
    mount_at(server, REQUEST_PATH, body).await;
}

async fn mount_at(server: &MockServer, request_path: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(request_path.to_owned()))
        .and(header("accept", REQUEST_MEDIA_TYPE))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

/// What asking for a request returns: tasks nested, objects listed twice.
fn request_response() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root tm:object_type="R" adtcore:name="AB1K900001" xmlns:tm="http://www.sap.com/cts/adt/tm" xmlns:adtcore="http://www.sap.com/adt/core">
  <tm:request tm:number="AB1K900001" tm:parent="" tm:owner="DEVELOPER" tm:desc="Widen the event log key" tm:type="K" tm:status="D" tm:status_text="Modifiable" tm:target="QA1" tm:target_desc="Quality">
    <tm:long_desc/>
    <tm:all_objects>
      <tm:abap_object tm:pgmid="R3TR" tm:type="PROG" tm:name="ZEXAMPLE_REPORT" tm:wbtype="PROG/P"/>
      <tm:abap_object tm:pgmid="R3TR" tm:type="CLAS" tm:name="ZCL_EXAMPLE" tm:wbtype="CLAS/OC"/>
    </tm:all_objects>
    <tm:task tm:number="AB1K900002" tm:parent="AB1K900001" tm:owner="DEVELOPER" tm:desc="Sample task" tm:type="Development/Correction" tm:status="D" tm:status_text="Modifiable">
      <tm:long_desc/>
      <tm:abap_object tm:pgmid="R3TR" tm:type="PROG" tm:name="ZEXAMPLE_REPORT" tm:wbtype="PROG/P"/>
      <tm:abap_object tm:pgmid="R3TR" tm:type="CLAS" tm:name="ZCL_EXAMPLE" tm:wbtype="CLAS/OC"/>
    </tm:task>
  </tm:request>
</tm:root>"#
}

/// What asking for a *task* returns: the parent's header, the task beside it.
fn task_response() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root tm:object_type="T" adtcore:name="AB1K900002" xmlns:tm="http://www.sap.com/cts/adt/tm" xmlns:adtcore="http://www.sap.com/adt/core">
  <tm:request tm:number="AB1K900001" tm:parent="" tm:owner="DEVELOPER" tm:desc="Widen the event log key" tm:type="K" tm:status="D" tm:status_text="Modifiable" tm:target="QA1">
    <tm:long_desc/>
  </tm:request>
  <tm:task tm:number="AB1K900002" tm:parent="AB1K900001" tm:owner="DEVELOPER" tm:desc="Sample task" tm:type="Development/Correction" tm:status="D" tm:status_text="Modifiable">
    <tm:long_desc/>
    <tm:abap_object tm:pgmid="R3TR" tm:type="PROG" tm:name="ZEXAMPLE_REPORT" tm:wbtype="PROG/P"/>
  </tm:task>
</tm:root>"#
}

#[tokio::test]
async fn reads_a_request_with_its_objects_and_tasks() {
    let server = MockServer::start().await;
    mount(&server, request_response()).await;

    let detail = show_transport_request(&client(&server), "AB1K900001")
        .await
        .unwrap();

    assert_eq!(detail.number, "AB1K900001");
    assert_eq!(
        detail.description.as_deref(),
        Some("Widen the event log key")
    );
    assert_eq!(detail.owner.as_deref(), Some("DEVELOPER"));
    assert_eq!(detail.status_text.as_deref(), Some("Modifiable"));
    assert_eq!(detail.target.as_deref(), Some("QA1"));

    // Two objects, not four: they appear in the aggregate *and* under the task
    // that owns them.
    assert_eq!(detail.objects.len(), 2);
    assert_eq!(detail.objects[0].name, "ZEXAMPLE_REPORT");
    assert_eq!(detail.objects[0].object_type.as_deref(), Some("PROG"));
    assert_eq!(detail.objects[0].program_id.as_deref(), Some("R3TR"));
    assert_eq!(detail.objects[0].workbench_type.as_deref(), Some("PROG/P"));
    assert_eq!(detail.objects[1].name, "ZCL_EXAMPLE");

    assert_eq!(detail.tasks.len(), 1);
    assert_eq!(detail.tasks[0].number, "AB1K900002");
    assert_eq!(
        detail.tasks[0].task_type.as_deref(),
        Some("Development/Correction")
    );
    assert_eq!(detail.tasks[0].objects.len(), 2);
    server.verify().await;
}

#[tokio::test]
async fn a_task_number_reads_its_parent_and_still_reports_the_objects() {
    // SAP resolves a task to its parent silently rather than refusing, and
    // sends no aggregate for that shape. Reading only tasks nested inside the
    // request would report the parent as holding nothing.
    let server = MockServer::start().await;
    // Asked for by *task* number, so that is the path SAP is called on.
    mount_at(
        &server,
        "/sap/bc/adt/cts/transportrequests/AB1K900002",
        task_response(),
    )
    .await;

    let detail = show_transport_request(&client(&server), "AB1K900002")
        .await
        .unwrap();

    // The number answered is the parent's, not the one asked for.
    assert_eq!(detail.number, "AB1K900001");
    assert_eq!(detail.tasks.len(), 1);
    assert_eq!(detail.tasks[0].number, "AB1K900002");
    assert_eq!(detail.objects.len(), 1);
    assert_eq!(detail.objects[0].name, "ZEXAMPLE_REPORT");
    server.verify().await;
}

#[tokio::test]
async fn a_request_holding_nothing_reports_no_objects_rather_than_failing() {
    // The shape a freshly created request has, observed live: SAP *omits*
    // `all_objects` rather than sending an empty one, so "no aggregate" is
    // the ordinary case for an empty request and not a signal of a truncated
    // response. Its task is present and empty.
    let server = MockServer::start().await;
    mount(
        &server,
        r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm">
  <tm:request tm:number="AB1K900001" tm:owner="DEVELOPER" tm:desc="Nothing in it yet" tm:type="K" tm:status="D" tm:status_text="Modifiable" tm:target="QA1">
    <tm:long_desc/>
    <tm:task tm:number="AB1K900002" tm:parent="AB1K900001" tm:owner="DEVELOPER" tm:desc="Nothing in it yet" tm:status="D">
      <tm:long_desc/>
    </tm:task>
  </tm:request>
</tm:root>"#,
    )
    .await;

    let detail = show_transport_request(&client(&server), "AB1K900001")
        .await
        .unwrap();

    assert!(detail.objects.is_empty());
    assert_eq!(detail.tasks.len(), 1);
    assert!(detail.tasks[0].objects.is_empty());
    // The request itself still reads normally.
    assert_eq!(detail.target.as_deref(), Some("QA1"));
    server.verify().await;
}

#[tokio::test]
async fn an_empty_aggregate_is_believed_rather_than_second_guessed() {
    // Not a shape a live system has been seen to send — SAP omits the wrapper
    // instead — but the two are distinct in the parser, and this pins which
    // one wins: a present-but-empty aggregate is SAP stating the request holds
    // nothing, so it is not overridden by scavenging the tasks.
    let server = MockServer::start().await;
    mount(
        &server,
        r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm">
  <tm:request tm:number="AB1K900001" tm:owner="DEVELOPER" tm:desc="Aggregate says empty" tm:type="K" tm:status="D">
    <tm:all_objects/>
    <tm:task tm:number="AB1K900002" tm:owner="DEVELOPER" tm:desc="Task disagrees" tm:status="D">
      <tm:abap_object tm:pgmid="R3TR" tm:type="PROG" tm:name="ZEXAMPLE_REPORT" tm:wbtype="PROG/P"/>
    </tm:task>
  </tm:request>
</tm:root>"#,
    )
    .await;

    let detail = show_transport_request(&client(&server), "AB1K900001")
        .await
        .unwrap();

    assert!(detail.objects.is_empty());
    // The task's own list is still reported as the task's.
    assert_eq!(detail.tasks[0].objects.len(), 1);
    server.verify().await;
}

#[tokio::test]
async fn an_object_recorded_in_two_tasks_is_counted_once() {
    // Only reachable on the no-aggregate shape, where the request's list has
    // to be built from the tasks. One object in two tasks is one object.
    let server = MockServer::start().await;
    mount(
        &server,
        r#"<?xml version="1.0" encoding="utf-8"?>
<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm">
  <tm:request tm:number="AB1K900001" tm:owner="DEVELOPER" tm:desc="Shared" tm:type="K" tm:status="D"/>
  <tm:task tm:number="AB1K900002" tm:owner="DEVELOPER" tm:desc="First" tm:status="D">
    <tm:abap_object tm:pgmid="R3TR" tm:type="PROG" tm:name="ZEXAMPLE_REPORT" tm:wbtype="PROG/P"/>
  </tm:task>
  <tm:task tm:number="AB1K900003" tm:owner="OTHER" tm:desc="Second" tm:status="D">
    <tm:abap_object tm:pgmid="R3TR" tm:type="PROG" tm:name="ZEXAMPLE_REPORT" tm:wbtype="PROG/P"/>
    <tm:abap_object tm:pgmid="R3TR" tm:type="CLAS" tm:name="ZCL_EXAMPLE" tm:wbtype="CLAS/OC"/>
  </tm:task>
</tm:root>"#,
    )
    .await;

    let detail = show_transport_request(&client(&server), "AB1K900001")
        .await
        .unwrap();

    assert_eq!(detail.tasks.len(), 2);
    assert_eq!(detail.objects.len(), 2);
    server.verify().await;
}

#[tokio::test]
async fn an_unknown_number_is_reported_as_missing_rather_than_as_a_transport_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(REQUEST_PATH))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            "<error><message>Resource Transport Request/Task AB1K900001 does not exist.</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let error = show_transport_request(&client(&server), "AB1K900001")
        .await
        .unwrap_err();

    assert!(matches!(error, TransportShowError::NotFound { .. }));
    assert_eq!(error.code(), "transport_not_found");
    assert!(error.hint().unwrap().contains("Check the number"));
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal transport list")
    );
    server.verify().await;
}

#[tokio::test]
async fn the_number_is_trimmed_and_upper_cased_before_it_is_requested() {
    let server = MockServer::start().await;
    mount(&server, request_response()).await;

    // Lower case and padded: the path mock above only matches the canonical
    // form, so this fails outright if the number is passed through as typed.
    show_transport_request(&client(&server), "  ab1k900001  ")
        .await
        .unwrap();

    server.verify().await;
}

#[tokio::test]
async fn a_blank_number_never_reaches_sap() {
    let server = MockServer::start().await;

    let error = show_transport_request(&client(&server), "   ")
        .await
        .unwrap_err();

    assert_eq!(error.code(), "blank_transport_number");
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal transport list")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_response_without_a_request_is_a_stable_parse_error() {
    let server = MockServer::start().await;
    mount(
        &server,
        r#"<tm:root xmlns:tm="http://www.sap.com/cts/adt/tm"/>"#,
    )
    .await;

    let error = show_transport_request(&client(&server), "AB1K900001")
        .await
        .unwrap_err();

    assert_eq!(error.code(), "transport_response_invalid");
    assert!(error.hint().is_some());
    server.verify().await;
}
