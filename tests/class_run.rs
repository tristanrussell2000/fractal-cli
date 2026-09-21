mod adt_edit_mock;

use adt_edit_mock::AdtEditSession;
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    class_run::{ClassRunError, run_class},
    client::SapClient,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

const SESSION: AdtEditSession = AdtEditSession {
    sap_client: "903",
    csrf_token: "class-run-token",
    session_cookie: "SAP_SESSIONID_CLASSRUN=run",
    object_path: "/sap/bc/adt/oo/classes/zcl_sample_probe",
    source_path: "/sap/bc/adt/oo/classes/zcl_sample_probe/source/main",
    lock_handle: "class-run-lock",
};

async fn client(server: &MockServer) -> SapClient {
    SESSION.mount_csrf_session(server).await;
    SapClient::new(
        &SESSION.profile(server.uri(), &["Z*"]),
        "password".to_owned(),
    )
    .unwrap()
}

#[tokio::test]
async fn posts_to_the_console_endpoint_and_returns_what_the_class_wrote() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/oo/classrun/ZCL_SAMPLE_PROBE"))
        .and(header("accept", "text/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string("rows=1\ncommitted=X\n"))
        .expect(1)
        .mount(&server)
        .await;

    let mut sap = client(&server).await;
    let result = run_class(&mut sap, "zcl_sample_probe").await.unwrap();

    assert_eq!(result.class_name, "ZCL_SAMPLE_PROBE");
    assert_eq!(result.output, "rows=1\ncommitted=X\n");
}

#[tokio::test]
async fn reports_a_class_that_does_not_implement_the_interface() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/oo/classrun/ZCL_SAMPLE_PROBE"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Resource not found"))
        .expect(1)
        .mount(&server)
        .await;

    let mut sap = client(&server).await;
    let error = run_class(&mut sap, "ZCL_SAMPLE_PROBE").await.unwrap_err();

    assert_eq!(error.code(), "class_run_failed");
    assert_eq!(error.status(), Some(404));
}

#[tokio::test]
async fn refuses_a_name_that_would_escape_the_endpoint_path() {
    let server = MockServer::start().await;
    let mut sap = SapClient::new(
        &SESSION.profile(server.uri(), &["Z*"]),
        "password".to_owned(),
    )
    .unwrap();

    let error = run_class(&mut sap, "ZCL_A/../../discovery?x=1")
        .await
        .unwrap_err();

    assert!(matches!(error, ClassRunError::InvalidName { .. }));
    assert_eq!(error.code(), "class_run_name_invalid");
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}
