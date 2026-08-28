use fractal::{
    config::Profile,
    sap::{
        adt_message_severity::AdtMessageSeverity,
        client::SapClient,
        editable_source::{AdtSourceVersion, EditableAdtObjectType, EditableAdtSourceTargetError},
        source_check::{AdtSourceCheckError, check_adt_stored_source},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string, header, method, path, query_param},
};

const OBJECT_URI: &str = "/sap/bc/adt/oo/classes/zcl_sample";
const SOURCE_URI: &str = "/sap/bc/adt/oo/classes/zcl_sample/source/main";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "100".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

async fn mount_csrf_session(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "check-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=check-test; Path=/"),
        )
        .expect(1)
        .mount(server)
        .await;
}

fn checkrun_body(version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><chkrun:checkObjectList xmlns:chkrun=\"http://www.sap.com/adt/checkrun\" xmlns:adtcore=\"http://www.sap.com/adt/core\"><chkrun:checkObject adtcore:uri=\"{SOURCE_URI}\" chkrun:version=\"{version}\"/></chkrun:checkObjectList>"
    )
}

async fn mount_checkrun(server: &MockServer, version: &str, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/checkruns"))
        .and(query_param("reporters", "abapCheckRun"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "check-csrf"))
        .and(header("cookie", "SAP_SESSIONID=check-test"))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.checkobjects+xml",
        ))
        .and(header(
            "accept",
            "application/vnd.sap.adt.checkmessages+xml",
        ))
        .and(body_string(checkrun_body(version)))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn checks_active_source_and_returns_structured_messages() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_checkrun(
        &server,
        "active",
        ResponseTemplate::new(200).set_body_string(
            r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun">
                <chkrun:checkMessage chkrun:type="E" chkrun:shortText="Expected &lt;identifier&gt;" chkrun:line="8"/>
                <chkrun:checkMessage chkrun:type="W" chkrun:shortText="Variable is unused" chkrun:line="11"/>
                <chkrun:checkMessage chkrun:type="I" chkrun:shortText="Check completed"/>
            </chkrun:checkMessageList>"#,
        ),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = check_adt_stored_source(
        &mut client,
        EditableAdtObjectType::Class,
        "zcl_sample",
        AdtSourceVersion::Active,
    )
    .await
    .unwrap();

    assert_eq!(result.identity.name, "ZCL_SAMPLE");
    assert_eq!(result.identity.object_uri, OBJECT_URI);
    assert_eq!(result.identity.source_uri, SOURCE_URI);
    assert!(result.check_executed);
    assert_eq!(result.inactive_version_exists, None);
    assert!(!result.clean);
    assert_eq!(result.errors, 1);
    assert_eq!(result.warnings, 1);
    assert_eq!(result.infos, 1);
    assert_eq!(result.messages[0].severity, AdtMessageSeverity::Error);
    assert_eq!(result.messages[0].text, "Expected <identifier>");
    assert_eq!(result.messages[0].line, Some(8));
    server.verify().await;
}

#[tokio::test]
async fn verifies_an_inactive_version_exists_before_checking_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .and(query_param("sap-client", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("<objects><object uri=\"{OBJECT_URI}\"/></objects>")),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_csrf_session(&server).await;
    mount_checkrun(
        &server,
        "inactive",
        ResponseTemplate::new(200).set_body_string(
            r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun"/>"#,
        ),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = check_adt_stored_source(
        &mut client,
        EditableAdtObjectType::Class,
        "ZCL_SAMPLE",
        AdtSourceVersion::Inactive,
    )
    .await
    .unwrap();

    assert_eq!(result.inactive_version_exists, Some(true));
    assert!(result.check_executed);
    assert!(result.clean);
    assert!(result.messages.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn does_not_run_a_fabricated_check_when_no_inactive_version_exists() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .and(query_param("sap-client", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<objects/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = check_adt_stored_source(
        &mut client,
        EditableAdtObjectType::Class,
        "ZCL_SAMPLE",
        AdtSourceVersion::Inactive,
    )
    .await
    .unwrap();

    assert_eq!(result.inactive_version_exists, Some(false));
    assert!(!result.check_executed);
    assert!(result.clean);
    assert_eq!(result.infos, 1);
    assert!(
        result.messages[0]
            .text
            .contains("No inactive version exists")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    server.verify().await;
}

#[tokio::test]
async fn rejects_malformed_checkrun_xml_with_a_stable_error() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_checkrun(
        &server,
        "active",
        ResponseTemplate::new(200).set_body_string("<not-closed>"),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = check_adt_stored_source(
        &mut client,
        EditableAdtObjectType::Class,
        "ZCL_SAMPLE",
        AdtSourceVersion::Active,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, AdtSourceCheckError::Parse { .. }));
    assert_eq!(error.code(), "edit_source_check_response_invalid");
    server.verify().await;
}

#[tokio::test]
async fn preserves_sap_failures_from_the_checkrun_request() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_checkrun(
        &server,
        "active",
        ResponseTemplate::new(403)
            .set_body_string("<error><message>Check authorization missing</message></error>"),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = check_adt_stored_source(
        &mut client,
        EditableAdtObjectType::Class,
        "ZCL_SAMPLE",
        AdtSourceVersion::Active,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, AdtSourceCheckError::Sap { .. }));
    // When the check cannot run, reading the version being checked is the
    // remaining way to inspect the source.
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal edit read --type CLAS --name ZCL_SAMPLE --version active")
    );
    assert_eq!(error.code(), "edit_source_check_failed");
    assert!(error.to_string().contains("Check authorization missing"));
    assert!(error.sap_error().is_some());
    server.verify().await;
}

#[tokio::test]
async fn rejects_an_invalid_object_name_before_any_http_request() {
    let mut client = SapClient::new(
        &profile("http://127.0.0.1:1".to_owned()),
        "password".to_owned(),
    )
    .unwrap();
    let error = check_adt_stored_source(
        &mut client,
        EditableAdtObjectType::Class,
        "ZCL;DELETE",
        AdtSourceVersion::Active,
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceCheckError::InvalidObject(EditableAdtSourceTargetError::InvalidObjectName(_))
    ));
    assert_eq!(error.code(), "invalid_edit_object_name");
}
