//! Running ABAP Unit tests.
//!
//! What matters: a missing object is refused before anything runs, because
//! SAP answers it exactly like an object without tests; and the run is sent
//! with the configuration and media types SAP accepts.

use std::time::Duration;

use fractal::config::Profile;
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    abap_unit::{
        RiskLevel, TestDuration, UnitRunRequest, UnitTarget, UnitTargetType, run_unit_tests,
    },
    client::SapClient,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

const CLASS: &str = "/sap/bc/adt/oo/classes/zsample_au";

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

fn request() -> UnitRunRequest {
    UnitRunRequest {
        targets: vec![UnitTarget {
            target_type: UnitTargetType::Class,
            name: "ZSAMPLE_AU".to_owned(),
            test_class: None,
            methods: Vec::new(),
        }],
        risk_levels: vec![RiskLevel::Harmless, RiskLevel::Dangerous],
        durations: vec![TestDuration::Short, TestDuration::Medium],
        timeout: Duration::from_secs(5),
    }
}

async fn mount_csrf(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "unit-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=unit; Path=/"),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn runs_with_the_configuration_sap_accepts_and_judges_the_answer() {
    let server = MockServer::start().await;
    mount_csrf(&server).await;
    Mock::given(method("GET"))
        .and(path(CLASS))
        .respond_with(ResponseTemplate::new(200).set_body_string("<class:abapClass/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/abapunit/testruns"))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.abapunit.testruns.config.v4+xml",
        ))
        .and(header(
            "accept",
            "application/vnd.sap.adt.abapunit.testruns.result.v2+xml",
        ))
        .and(body_string_contains(format!("adtcore:uri=\"{CLASS}\"")))
        .and(body_string_contains(r#"critical="false""#))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<aunit:runResult xmlns:aunit="http://www.sap.com/adt/aunit"><program adtcore:name="ZSAMPLE_AU" xmlns:adtcore="http://www.sap.com/adt/core"><testClasses><testClass adtcore:name="LTC_ADD"><testMethods><testMethod adtcore:name="PASSES"/></testMethods></testClass></testClasses></program></aunit:runResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let summary = run_unit_tests(&mut client, &request()).await.unwrap();

    assert!(summary.green);
    assert_eq!(summary.totals.passed, 1);
    server.verify().await;
}

#[tokio::test]
async fn a_missing_object_is_refused_before_anything_runs() {
    // SAP answers a nonexistent object with an empty result, the same bytes
    // as an object without tests.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLASS))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/abapunit/testruns"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = run_unit_tests(&mut client, &request()).await.unwrap_err();

    assert_eq!(error.code(), "unit_test_target_not_found");
    server.verify().await;
}

#[tokio::test]
async fn an_unreadable_object_is_an_error_not_an_absence() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CLASS))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = run_unit_tests(&mut client, &request()).await.unwrap_err();

    assert_eq!(error.code(), "unit_test_target_unreadable");
}
