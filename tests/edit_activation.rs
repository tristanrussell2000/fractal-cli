use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use fractal::{
    config::Profile,
    sap::{
        adt_message_severity::AdtMessageSeverity,
        client::SapClient,
        editable_source::EditableAdtObjectType,
        source_activation::{
            AdtSourceActivationError, AdtSourceActivationRequest, activate_adt_source,
        },
    },
    source_change::source_sha256,
};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{body_string, header, method, path, query_param, query_param_is_missing},
};

const OBJECT_URI: &str = "/sap/bc/adt/oo/classes/zcl_sample";
const SOURCE_URI: &str = "/sap/bc/adt/oo/classes/zcl_sample/source/main";
const SOURCE: &str = "CLASS zcl_sample DEFINITION.\nENDCLASS.\n";
const LOCK_HANDLE: &str = "202608231234567890";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "100".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

fn activation_request(transport: Option<&str>) -> AdtSourceActivationRequest {
    AdtSourceActivationRequest {
        object_type: EditableAdtObjectType::Class,
        name: "zcl_sample".to_owned(),
        transport: transport.map(str::to_owned),
    }
}

async fn mount_csrf_session(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "activation-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=activation-test; Path=/"),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_source_read(server: &MockServer, version: &str, source: &'static str) {
    mount_source_read_response(
        server,
        version,
        ResponseTemplate::new(200).set_body_string(source),
    )
    .await;
}

async fn mount_source_read_response(
    server: &MockServer,
    version: &str,
    response: ResponseTemplate,
) {
    Mock::given(method("GET"))
        .and(path(SOURCE_URI))
        .and(query_param("version", version))
        .and(query_param("sap-client", "100"))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

fn checkrun_body() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><chkrun:checkObjectList xmlns:chkrun=\"http://www.sap.com/adt/checkrun\" xmlns:adtcore=\"http://www.sap.com/adt/core\"><chkrun:checkObject adtcore:uri=\"{SOURCE_URI}\" chkrun:version=\"inactive\"/></chkrun:checkObjectList>"
    )
}

async fn mount_checkrun(server: &MockServer, response: &'static str) {
    mount_checkrun_response(server, ResponseTemplate::new(200).set_body_string(response)).await;
}

async fn mount_checkrun_response(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/checkruns"))
        .and(query_param("reporters", "abapCheckRun"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "activation-csrf"))
        .and(header("cookie", "SAP_SESSIONID=activation-test"))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.checkobjects+xml",
        ))
        .and(header(
            "accept",
            "application/vnd.sap.adt.checkmessages+xml",
        ))
        .and(body_string(checkrun_body()))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

fn activation_body() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\"><adtcore:objectReference adtcore:uri=\"{OBJECT_URI}\" adtcore:name=\"ZCL_SAMPLE\"/></adtcore:objectReferences>"
    )
}

async fn mount_activation(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/activation"))
        .and(query_param("method", "activate"))
        .and(query_param("preauditRequested", "true"))
        .and(query_param("sap-client", "100"))
        .and(query_param_is_missing("corrNr"))
        .and(header("x-csrf-token", "activation-csrf"))
        .and(header("cookie", "SAP_SESSIONID=activation-test"))
        .and(header("content-type", "application/xml"))
        .and(header("accept", "application/xml"))
        .and(body_string(activation_body()))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

#[derive(Clone)]
struct InactiveObjectSequence {
    calls: Arc<AtomicUsize>,
    remains_after_activation: bool,
    post_activation_delay: Duration,
}

impl Respond for InactiveObjectSequence {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let exists = call == 0 || self.remains_after_activation;
        let body = if exists {
            format!("<objects><object uri=\"{OBJECT_URI}\"/></objects>")
        } else {
            "<objects/>".to_owned()
        };
        let response = ResponseTemplate::new(200).set_body_string(body);
        if call > 0 && !self.post_activation_delay.is_zero() {
            response.set_delay(self.post_activation_delay)
        } else {
            response
        }
    }
}

async fn mount_inactive_sequence(server: &MockServer, remains_after_activation: bool) {
    mount_inactive_sequence_with_post_delay(server, remains_after_activation, Duration::ZERO).await;
}

async fn mount_inactive_sequence_with_post_delay(
    server: &MockServer,
    remains_after_activation: bool,
    post_activation_delay: Duration,
) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .and(query_param("sap-client", "100"))
        .respond_with(InactiveObjectSequence {
            calls: Arc::new(AtomicUsize::new(0)),
            remains_after_activation,
            post_activation_delay,
        })
        .expect(2)
        .mount(server)
        .await;
}

async fn mount_clean_preflight(server: &MockServer) {
    mount_inactive_sequence(server, false).await;
    mount_source_read(server, "inactive", SOURCE).await;
    mount_csrf_session(server).await;
    mount_checkrun(
        server,
        r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun">
            <chkrun:checkMessage chkrun:type="W" chkrun:shortText="Obsolete statement" chkrun:line="5"/>
        </chkrun:checkMessageList>"#,
    )
    .await;
}

async fn mount_inputs_until_activation(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .and(query_param("sap-client", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("<objects><object uri=\"{OBJECT_URI}\"/></objects>")),
        )
        .expect(1)
        .mount(server)
        .await;
    mount_source_read(server, "inactive", SOURCE).await;
    mount_csrf_session(server).await;
    mount_checkrun(server, "<checkMessageList/>").await;
}

#[tokio::test]
async fn activates_and_verifies_the_exact_inactive_source() {
    let server = MockServer::start().await;
    mount_clean_preflight(&server).await;
    mount_activation(
        &server,
        ResponseTemplate::new(200).set_body_string(
            r#"<act:activationResult xmlns:act="http://www.sap.com/adt/activation" act:activationExecuted="true">
                <act:msg act:type="I" act:objDescr="Class ZCL_SAMPLE"><act:shortText><act:txt>Activation completed</act:txt></act:shortText></act:msg>
            </act:activationResult>"#,
        ),
    )
    .await;
    mount_source_read(&server, "active", SOURCE).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = activate_adt_source(&mut client, &["Z*".to_owned()], &activation_request(None))
        .await
        .unwrap();

    assert_eq!(result.identity.name, "ZCL_SAMPLE");
    assert_eq!(result.identity.object_uri, OBJECT_URI);
    assert_eq!(result.inactive.sha256, source_sha256(SOURCE));
    assert_eq!(result.active.sha256, result.inactive.sha256);
    assert_eq!(result.sap_reported_activation_executed, Some(true));
    assert!(result.activation_response_parsed);
    assert_eq!(result.precheck.warnings, 1);
    assert_eq!(result.activation_messages.len(), 1);
    assert_eq!(
        result.activation_messages[0].severity,
        AdtMessageSeverity::Info
    );
    server.verify().await;
}

#[tokio::test]
async fn reads_inactive_source_and_runs_precheck_concurrently() {
    let server = MockServer::start().await;
    mount_inactive_sequence(&server, false).await;
    mount_source_read_response(
        &server,
        "inactive",
        ResponseTemplate::new(200)
            .set_body_string(SOURCE)
            .set_delay(Duration::from_millis(500)),
    )
    .await;
    mount_csrf_session(&server).await;
    mount_checkrun_response(
        &server,
        ResponseTemplate::new(200)
            .set_body_string("<checkMessageList/>")
            .set_delay(Duration::from_millis(500)),
    )
    .await;
    mount_activation(
        &server,
        ResponseTemplate::new(200)
            .set_body_string(r#"<activationResult activationExecuted="true"/>"#),
    )
    .await;
    mount_source_read(&server, "active", SOURCE).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let started = Instant::now();
    activate_adt_source(&mut client, &["Z*".to_owned()], &activation_request(None))
        .await
        .unwrap();

    assert!(started.elapsed() < Duration::from_millis(850));
    server.verify().await;
}

#[tokio::test]
async fn reads_active_source_and_probes_inactive_state_concurrently() {
    let server = MockServer::start().await;
    mount_inactive_sequence_with_post_delay(&server, false, Duration::from_millis(500)).await;
    mount_source_read(&server, "inactive", SOURCE).await;
    mount_csrf_session(&server).await;
    mount_checkrun(&server, "<checkMessageList/>").await;
    mount_activation(
        &server,
        ResponseTemplate::new(200)
            .set_body_string(r#"<activationResult activationExecuted="true"/>"#),
    )
    .await;
    mount_source_read_response(
        &server,
        "active",
        ResponseTemplate::new(200)
            .set_body_string(SOURCE)
            .set_delay(Duration::from_millis(500)),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let started = Instant::now();
    activate_adt_source(&mut client, &["Z*".to_owned()], &activation_request(None))
        .await
        .unwrap();

    assert!(started.elapsed() < Duration::from_millis(850));
    server.verify().await;
}

#[tokio::test]
async fn refuses_to_activate_when_no_inactive_version_exists() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<objects/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = activate_adt_source(&mut client, &["Z*".to_owned()], &activation_request(None))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceActivationError::NoInactiveVersion { .. }
    ));
    assert_eq!(error.code(), "edit_activation_no_inactive_source");
    assert!(
        error
            .hint()
            .contains("fractal edit read --type CLAS --name ZCL_SAMPLE --version active"),
        "hint should name the object to inspect: {}",
        error.hint()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    server.verify().await;
}

#[tokio::test]
async fn syntax_errors_stop_before_transport_or_activation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("<objects><object uri=\"{OBJECT_URI}\"/></objects>")),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_source_read(&server, "inactive", SOURCE).await;
    mount_csrf_session(&server).await;
    mount_checkrun(
        &server,
        r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkMessage chkrun:type="E" chkrun:shortText="Expected identifier" chkrun:line="3"/></chkrun:checkMessageList>"#,
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = activate_adt_source(
        &mut client,
        &["Z*".to_owned()],
        &activation_request(Some("DE3K900575")),
    )
    .await
    .unwrap_err();

    assert!(
        error
            .hint()
            .contains("fractal edit check --type CLAS --name ZCL_SAMPLE --version inactive"),
        "precheck hint should name a runnable check command: {}",
        error.hint()
    );
    let AdtSourceActivationError::PrecheckRejected {
        errors, messages, ..
    } = error
    else {
        panic!("expected rejected precheck");
    };
    assert_eq!(errors, 1);
    assert_eq!(messages[0].text, "Expected identifier");
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
    server.verify().await;
}

#[tokio::test]
async fn reports_http_200_activation_refusal_and_messages() {
    let server = MockServer::start().await;
    mount_inactive_sequence(&server, true).await;
    mount_source_read(&server, "inactive", SOURCE).await;
    mount_csrf_session(&server).await;
    mount_checkrun(&server, "<checkMessageList/>").await;
    mount_activation(
        &server,
        ResponseTemplate::new(200).set_body_string(
            r#"<activationResult activationExecuted="false"><msg type="E" objDescr="Class ZCL_SAMPLE"><shortText><txt>Resource is not locked in a transport request</txt></shortText></msg></activationResult>"#,
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(SOURCE_URI))
        .and(query_param("version", "active"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SOURCE)
                .set_delay(Duration::from_secs(5)),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        activate_adt_source(&mut client, &["Z*".to_owned()], &activation_request(None)),
    )
    .await
    .expect("inactive refusal should not wait for the delayed active source")
    .unwrap_err();

    let AdtSourceActivationError::ActivationRefused {
        sap_reported_activation_executed,
        messages,
    } = error
    else {
        panic!("expected activation refusal");
    };
    assert_eq!(sap_reported_activation_executed, Some(false));
    assert_eq!(messages.len(), 1);
    assert!(messages[0].text.contains("not locked in a transport"));
    server.verify().await;
}

#[tokio::test]
async fn preserves_activation_http_failures_as_a_distinct_stage() {
    let server = MockServer::start().await;
    mount_inputs_until_activation(&server).await;
    mount_activation(
        &server,
        ResponseTemplate::new(403)
            .set_body_string("<error><message>Activation authorization missing</message></error>"),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = activate_adt_source(&mut client, &["Z*".to_owned()], &activation_request(None))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceActivationError::ActivationRequest { .. }
    ));
    assert_eq!(error.code(), "edit_activation_request_failed");
    assert!(
        error
            .to_string()
            .contains("Activation authorization missing")
    );
    server.verify().await;
}

#[tokio::test]
async fn transport_attachment_failure_stops_before_activation() {
    let server = MockServer::start().await;
    mount_inputs_until_activation(&server).await;
    Mock::given(method("POST"))
        .and(path(OBJECT_URI))
        .and(query_param("_action", "LOCK"))
        .and(query_param("corrNr", "DE3K900575"))
        .respond_with(ResponseTemplate::new(409).set_body_string(
            "<error><message>Object is already locked in request DE3K900999</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = activate_adt_source(
        &mut client,
        &["Z*".to_owned()],
        &activation_request(Some("DE3K900575")),
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceActivationError::TransportAttachment(_)
    ));
    assert_eq!(error.code(), "edit_activation_transport_failed");
    assert!(error.hint().contains("DE3K900999"));
    server.verify().await;
}

#[tokio::test]
async fn distinguishes_post_activation_source_mismatch() {
    const DIFFERENT_ACTIVE: &str = "CLASS zcl_sample DEFINITION PUBLIC.\nENDCLASS.\n";
    let server = MockServer::start().await;
    mount_clean_preflight(&server).await;
    mount_activation(
        &server,
        ResponseTemplate::new(200)
            .set_body_string(r#"<activationResult activationExecuted="true"/>"#),
    )
    .await;
    mount_source_read(&server, "active", DIFFERENT_ACTIVE).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = activate_adt_source(&mut client, &["Z*".to_owned()], &activation_request(None))
        .await
        .unwrap_err();

    let AdtSourceActivationError::VerificationMismatch {
        inactive_sha256,
        active_sha256,
        ..
    } = error
    else {
        panic!("expected verification mismatch");
    };
    assert_eq!(inactive_sha256, source_sha256(SOURCE));
    assert_eq!(active_sha256, source_sha256(DIFFERENT_ACTIVE));
    server.verify().await;
}

#[tokio::test]
async fn attaches_the_parent_transport_before_activation() {
    let server = MockServer::start().await;
    mount_clean_preflight(&server).await;
    Mock::given(method("POST"))
        .and(path(OBJECT_URI))
        .and(query_param("_action", "LOCK"))
        .and(query_param("accessMode", "MODIFY"))
        .and(query_param("corrNr", "DE3K900575"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<lockResult><LOCK_HANDLE>{LOCK_HANDLE}</LOCK_HANDLE></lockResult>"
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(OBJECT_URI))
        .and(query_param("_action", "UNLOCK"))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(query_param_is_missing("corrNr"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    mount_activation(
        &server,
        ResponseTemplate::new(200)
            .set_body_string(r#"<activationResult activationExecuted="true"/>"#),
    )
    .await;
    mount_source_read(&server, "active", SOURCE).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = activate_adt_source(
        &mut client,
        &["Z*".to_owned()],
        &activation_request(Some(" de3k900575 ")),
    )
    .await
    .unwrap();

    assert_eq!(result.transport.as_deref(), Some("DE3K900575"));
    let paths = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|request| request.url.path().to_owned())
        .collect::<Vec<_>>();
    let lock_position = paths.iter().position(|path| path == OBJECT_URI).unwrap();
    let activation_position = paths
        .iter()
        .position(|path| path == "/sap/bc/adt/activation")
        .unwrap();
    assert!(lock_position < activation_position);
    server.verify().await;
}

#[tokio::test]
async fn verified_activation_survives_an_unparseable_success_body() {
    let server = MockServer::start().await;
    mount_clean_preflight(&server).await;
    mount_activation(
        &server,
        ResponseTemplate::new(200).set_body_string("not XML"),
    )
    .await;
    mount_source_read(&server, "active", SOURCE).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = activate_adt_source(&mut client, &["Z*".to_owned()], &activation_request(None))
        .await
        .unwrap();

    assert!(!result.activation_response_parsed);
    assert_eq!(result.sap_reported_activation_executed, None);
    assert!(result.activation_messages.is_empty());
    server.verify().await;
}
