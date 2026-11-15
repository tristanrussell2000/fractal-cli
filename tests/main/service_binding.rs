//! Publishing a service binding.
//!
//! The assertions that matter most are the requests that are **not** sent: an
//! already-published service, an inactive binding and a dry run all stop
//! before the POST. And a publish answered `OK` is not a success until the
//! details document says so.

use crate::adt_edit_mock::AdtEditSession;
use fractal::config::{EditPolicy, Profile};
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    client::SapClient,
    service_binding::{
        ODataVersion, PublishOperation, PublishRequest, PublishStatus, get_from_service,
        publish_service_binding, show_service_binding,
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

const BINDING_PATH: &str = "/sap/bc/adt/businessservices/bindings/zsample_ui_o4";
const V4_DETAILS_PATH: &str = "/sap/bc/adt/businessservices/odatav4/ZSAMPLE_UI_O4";
const V4_PUBLISH_PATH: &str = "/sap/bc/adt/businessservices/odatav4/publishjobs";
const V4_UNPUBLISH_PATH: &str = "/sap/bc/adt/businessservices/odatav4/unpublishjobs";
const V2_DETAILS_PATH: &str = "/sap/bc/adt/businessservices/odatav2/ZSAMPLE_UI_O4";
const V2_PUBLISH_PATH: &str = "/sap/bc/adt/businessservices/odatav2/publishjobs";

fn session() -> AdtEditSession {
    AdtEditSession {
        sap_client: "100",
        csrf_token: "binding-publish-token",
        session_cookie: "SAP_SESSIONID=binding-publish",
        object_path: BINDING_PATH,
        source_path: "",
        lock_handle: "binding-publish-lock",
    }
}

fn profile(base_url: String) -> Profile {
    session().profile(base_url, &["Z*"])
}

async fn client(server: &MockServer) -> SapClient {
    SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap()
}

fn policy() -> EditPolicy {
    EditPolicy::namespaces_only(&["Z*"])
}

fn binding(version: &str, protocol: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><srvb:serviceBinding srvb:published="false" adtcore:name="ZSAMPLE_UI_O4" adtcore:type="SRVB/SVB" adtcore:version="{version}" xmlns:srvb="http://www.sap.com/adt/ddic/ServiceBindings" xmlns:adtcore="http://www.sap.com/adt/core"><adtcore:packageRef adtcore:name="$TMP"/><srvb:services srvb:name="ZSAMPLE_UI_O4"><srvb:content srvb:version="0001"><srvb:serviceDefinition adtcore:name="ZSAMPLE_SD"/></srvb:content></srvb:services><srvb:binding srvb:type="ODATA" srvb:version="{protocol}" srvb:category="0"/></srvb:serviceBinding>"#
    )
}

fn v4_details(published: bool) -> String {
    let url = if published {
        "/sap/opu/odata4/sap/zsample_ui_o4/srvd/sap/zsample_ui_o4/0001/"
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><odatav4:serviceGroup odatav4:published="{published}" xmlns:odatav4="http://www.sap.com/categories/odatav4" xmlns:adtcore="http://www.sap.com/adt/core"><odatav4:services odatav4:serviceId="ZSAMPLE_UI_O4" odatav4:serviceVersion="0001" odatav4:serviceUrl="{url}" odatav4:annotationUrl=""><serviceInfo:serviceInformation xmlns:serviceInfo="http://www.sap.com/categories/serviceinformation"/></odatav4:services></odatav4:serviceGroup>"#
    )
}

fn v2_details(published: bool) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><odatav2:serviceList xmlns:odatav2="http://www.sap.com/categories/odatav2"><odatav2:services odatav2:serviceId="ZSAMPLE_UI_O4" odatav2:serviceVersion="0001" odatav2:serviceUrl="" odatav2:published="{published}"/></odatav2:serviceList>"#
    )
}

fn job_answer(severity: &str, short: &str, long: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><asx:abap version="1.0" xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA><SEVERITY>{severity}</SEVERITY><SHORT_TEXT>{short}</SHORT_TEXT><LONG_TEXT>{long}</LONG_TEXT></DATA></asx:values></asx:abap>"#
    )
}

async fn mount_binding(server: &MockServer, version: &str, protocol: &str) {
    Mock::given(method("GET"))
        .and(path(BINDING_PATH))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_string(binding(version, protocol)))
        .mount(server)
        .await;
}

/// The details document read before the POST and again after it.
async fn mount_details_before_and_after(
    server: &MockServer,
    details_path: &str,
    before: String,
    after: Option<String>,
) {
    let read = || {
        Mock::given(method("GET"))
            .and(path(details_path))
            .and(query_param("servicename", "ZSAMPLE_UI_O4"))
            .and(query_param("serviceversion", "0001"))
            .and(query_param("srvdname", "ZSAMPLE_SD"))
    };
    read()
        .respond_with(ResponseTemplate::new(200).set_body_string(before))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
    if let Some(after) = after {
        read()
            .respond_with(ResponseTemplate::new(200).set_body_string(after))
            .expect(1)
            .mount(server)
            .await;
    }
}

async fn mount_job(server: &MockServer, job_path: &str, answer: String, times: u64) {
    Mock::given(method("POST"))
        .and(path(job_path))
        .and(query_param("servicename", "ZSAMPLE_UI_O4"))
        .and(query_param("serviceversion", "0001"))
        .and(header("content-type", "application/xml"))
        .and(body_string_contains(
            r#"<adtcore:objectReference adtcore:type="SRVB/SVB" adtcore:name="ZSAMPLE_UI_O4"/>"#,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(answer))
        .expect(times)
        .mount(server)
        .await;
}

fn request(operation: PublishOperation) -> PublishRequest {
    PublishRequest {
        operation,
        name: "zsample_ui_o4".to_owned(),
        service_version: None,
        dry_run: false,
    }
}

#[tokio::test]
async fn publishes_a_v4_binding_and_proves_it_from_the_details() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(
        &server,
        V4_DETAILS_PATH,
        v4_details(false),
        Some(v4_details(true)),
    )
    .await;
    mount_job(
        &server,
        V4_PUBLISH_PATH,
        job_answer("OK", "ZSAMPLE_UI_O4 published locally", ""),
        1,
    )
    .await;

    let result = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &request(PublishOperation::Publish),
    )
    .await
    .unwrap();

    assert_eq!(result.status, PublishStatus::Verified);
    assert_eq!(result.protocol, ODataVersion::V4);
    assert_eq!(result.request_path, V4_PUBLISH_PATH);
    assert_eq!(
        result.details_after.service_url.as_deref(),
        Some("/sap/opu/odata4/sap/zsample_ui_o4/srvd/sap/zsample_ui_o4/0001/")
    );
    assert_eq!(
        result.sap_message.unwrap().short_text,
        "ZSAMPLE_UI_O4 published locally"
    );
    server.verify().await;
}

#[tokio::test]
async fn a_v2_binding_publishes_through_the_v2_endpoints() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    mount_binding(&server, "active", "V2").await;
    mount_details_before_and_after(
        &server,
        V2_DETAILS_PATH,
        v2_details(false),
        Some(v2_details(true)),
    )
    .await;
    mount_job(
        &server,
        V2_PUBLISH_PATH,
        job_answer("OK", "activated locally", ""),
        1,
    )
    .await;

    let result = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &request(PublishOperation::Publish),
    )
    .await
    .unwrap();

    assert_eq!(result.status, PublishStatus::Verified);
    assert_eq!(result.protocol, ODataVersion::V2);
    server.verify().await;
}

#[tokio::test]
async fn unpublishes_through_the_unpublish_endpoint() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(
        &server,
        V4_DETAILS_PATH,
        v4_details(true),
        Some(v4_details(false)),
    )
    .await;
    mount_job(
        &server,
        V4_UNPUBLISH_PATH,
        job_answer("OK", "un-published locally", ""),
        1,
    )
    .await;

    let result = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &request(PublishOperation::Unpublish),
    )
    .await
    .unwrap();

    assert_eq!(result.status, PublishStatus::Verified);
    assert!(!result.details_after.published);
    server.verify().await;
}

#[tokio::test]
async fn an_already_published_service_sends_nothing() {
    let server = MockServer::start().await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(&server, V4_DETAILS_PATH, v4_details(true), None).await;
    mount_job(&server, V4_PUBLISH_PATH, job_answer("OK", "", ""), 0).await;

    let result = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &request(PublishOperation::Publish),
    )
    .await
    .unwrap();

    assert_eq!(result.status, PublishStatus::AlreadyDone);
    server.verify().await;
}

#[tokio::test]
async fn a_dry_run_reads_and_sends_nothing() {
    let server = MockServer::start().await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(&server, V4_DETAILS_PATH, v4_details(false), None).await;
    mount_job(&server, V4_PUBLISH_PATH, job_answer("OK", "", ""), 0).await;

    let result = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &PublishRequest {
            dry_run: true,
            ..request(PublishOperation::Publish)
        },
    )
    .await
    .unwrap();

    assert_eq!(result.status, PublishStatus::WouldSend);
    assert_eq!(result.request_path, V4_PUBLISH_PATH);
    server.verify().await;
}

#[tokio::test]
async fn an_inactive_binding_is_refused_before_anything_is_sent() {
    // SAP serves the inactive document when no active one exists, and its own
    // publish answer for this case is a misleading "does not exist".
    let server = MockServer::start().await;
    mount_binding(&server, "inactive", "V4").await;
    mount_job(&server, V4_PUBLISH_PATH, job_answer("OK", "", ""), 0).await;

    let error = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &request(PublishOperation::Publish),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "binding_not_active");
    assert!(error.hint().unwrap().contains("edit activate --type SRVB"));
    server.verify().await;
}

#[tokio::test]
async fn a_refusal_inside_a_200_is_a_failure_with_sap_s_reason() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    mount_binding(&server, "active", "V2").await;
    mount_details_before_and_after(&server, V2_DETAILS_PATH, v2_details(false), None).await;
    mount_job(
        &server,
        V2_PUBLISH_PATH,
        job_answer(
            "ERROR",
            "Activating Local Service Endpoint of service ZSAMPLE_UI_O4 with version 0001 failed",
            "Metadata not loaded - See /IWBEP/ERROR_LOG",
        ),
        1,
    )
    .await;

    let error = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &request(PublishOperation::Publish),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "binding_publish_failed");
    assert!(error.hint().unwrap().contains("Metadata not loaded"));
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal binding show --name ZSAMPLE_UI_O4")
    );
    server.verify().await;
}

#[tokio::test]
async fn an_ok_answer_the_details_do_not_confirm_is_unverified() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(
        &server,
        V4_DETAILS_PATH,
        v4_details(false),
        Some(v4_details(false)),
    )
    .await;
    mount_job(
        &server,
        V4_PUBLISH_PATH,
        job_answer("OK", "published locally", ""),
        1,
    )
    .await;

    let error = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &request(PublishOperation::Publish),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "binding_publish_unverified");
    server.verify().await;
}

#[tokio::test]
async fn show_reports_both_flags_without_sending_anything() {
    let server = MockServer::start().await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(&server, V4_DETAILS_PATH, v4_details(true), None).await;

    let result = show_service_binding(&client(&server).await, "ZSAMPLE_UI_O4")
        .await
        .unwrap();

    // The binding says false, the registration says true: both are reported.
    assert_eq!(result.binding_published_flag, Some(false));
    assert!(result.services[0].details.published);
    server.verify().await;
}

#[tokio::test]
async fn a_missing_binding_points_at_search() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(BINDING_PATH))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let error = show_service_binding(&client(&server).await, "ZSAMPLE_UI_O4")
        .await
        .unwrap_err();

    assert_eq!(error.code(), "binding_not_found");
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal object search ZSAMPLE_UI_O4 --kind SRVB")
    );
}

const V4_ROOT: &str = "/sap/opu/odata4/sap/zsample_ui_o4/srvd/sap/zsample_ui_o4/0001/";

#[tokio::test]
async fn get_reads_the_callers_path_under_the_registered_root() {
    let server = MockServer::start().await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(&server, V4_DETAILS_PATH, v4_details(true), None).await;
    Mock::given(method("GET"))
        .and(path(format!("{V4_ROOT}Language")))
        .and(query_param("$top", "2"))
        .and(query_param("$filter", "IsoCode eq 'DE'"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"value":[{"Language":"DE","IsoCode":"DE"}]}"#,
            "application/json; charset=utf-8",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let result = get_from_service(
        &client(&server).await,
        "ZSAMPLE_UI_O4",
        None,
        "Language?$top=2&$filter=IsoCode eq 'DE'",
    )
    .await
    .unwrap();

    assert_eq!(
        result.url,
        format!("{V4_ROOT}Language?$top=2&$filter=IsoCode eq 'DE'")
    );
    assert_eq!(result.status, 200);
    assert!(result.content_type.unwrap().contains("json"));
    assert!(result.body.contains("\"IsoCode\":\"DE\""));
    server.verify().await;
}

#[tokio::test]
async fn get_joins_onto_a_v2_root_that_has_no_trailing_slash() {
    let server = MockServer::start().await;
    mount_binding(&server, "active", "V2").await;
    let details = v2_details(true).replace(
        r#"odatav2:serviceUrl="""#,
        r#"odatav2:serviceUrl="/sap/opu/odata/sap/ZSAMPLE_UI_O4""#,
    );
    mount_details_before_and_after(&server, V2_DETAILS_PATH, details, None).await;
    Mock::given(method("GET"))
        .and(path("/sap/opu/odata/sap/ZSAMPLE_UI_O4/$metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<edmx:Edmx/>"))
        .expect(1)
        .mount(&server)
        .await;

    let result = get_from_service(&client(&server).await, "ZSAMPLE_UI_O4", None, "$metadata")
        .await
        .unwrap();

    assert_eq!(result.protocol, ODataVersion::V2);
    assert_eq!(result.body, "<edmx:Edmx/>");
    server.verify().await;
}

#[tokio::test]
async fn get_refuses_an_unpublished_service_without_calling_it() {
    // An unpublished V2 service answers 403, which reads like an
    // authorization problem. The registration says what is really wrong.
    let server = MockServer::start().await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(&server, V4_DETAILS_PATH, v4_details(false), None).await;
    Mock::given(method("GET"))
        .and(path(format!("{V4_ROOT}Language")))
        .respond_with(ResponseTemplate::new(403))
        .expect(0)
        .mount(&server)
        .await;

    let error = get_from_service(&client(&server).await, "ZSAMPLE_UI_O4", None, "Language")
        .await
        .unwrap_err();

    assert_eq!(error.code(), "binding_not_published");
    assert!(
        error
            .hint()
            .unwrap()
            .contains("fractal binding publish --name ZSAMPLE_UI_O4")
    );
    server.verify().await;
}

#[tokio::test]
async fn get_refuses_a_path_outside_the_root_before_any_request() {
    let server = MockServer::start().await;

    let error = get_from_service(
        &client(&server).await,
        "ZSAMPLE_UI_O4",
        None,
        "../../../bc/adt/discovery",
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "binding_path_invalid");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn get_carries_the_services_own_error_message() {
    let server = MockServer::start().await;
    mount_binding(&server, "active", "V4").await;
    mount_details_before_and_after(&server, V4_DETAILS_PATH, v4_details(true), None).await;
    Mock::given(method("GET"))
        .and(path(format!("{V4_ROOT}Language")))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"code":"/IWBEP/CM_V4_RUNTIME/021","message":"Property 'Nope' is unknown"}}"#,
        ))
        .mount(&server)
        .await;

    let error = get_from_service(
        &client(&server).await,
        "ZSAMPLE_UI_O4",
        None,
        "Language?$filter=Nope eq 1",
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "binding_get_failed");
    assert_eq!(error.status(), Some(400));
    assert!(
        error.message().contains("Property 'Nope' is unknown"),
        "{}",
        error.message()
    );
}

#[tokio::test]
async fn any_binding_can_be_read_but_only_the_customer_namespace_published() {
    let server = MockServer::start().await;
    let sap_binding = "/sap/bc/adt/businessservices/bindings/sap_ui_o4";
    Mock::given(method("GET"))
        .and(path(sap_binding))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(binding("active", "V4").replace("ZSAMPLE_UI_O4", "SAP_UI_O4")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/businessservices/odatav4/SAP_UI_O4"))
        .respond_with(ResponseTemplate::new(200).set_body_string(v4_details(false)))
        .mount(&server)
        .await;

    let shown = show_service_binding(&client(&server).await, "SAP_UI_O4")
        .await
        .unwrap();
    assert_eq!(shown.name, "SAP_UI_O4");

    let refused = publish_service_binding(
        &mut client(&server).await,
        &policy(),
        &PublishRequest {
            name: "SAP_UI_O4".to_owned(),
            ..request(PublishOperation::Publish)
        },
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code(), "object_outside_customer_namespaces");
}
