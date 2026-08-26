use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        edit_session::AdtEditSessionError,
        editable_source::{AdtEditTargetValidationError, EditableAdtObjectType},
        source_activation::AdtSourceActivationError,
        source_discard::{
            AdtInactiveSourceDiscardError, AdtInactiveSourceDiscardRequest,
            discard_inactive_adt_source,
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
const ACTIVE_SOURCE: &str = "CLASS zcl_sample DEFINITION.\nENDCLASS.\n";
const INACTIVE_SOURCE: &str = "CLASS zcl_sample DEFINITION PUBLIC.\nENDCLASS.\n";
const LOCK_HANDLE: &str = "202608241234567890";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "100".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

fn discard_request(transport: Option<&str>) -> AdtInactiveSourceDiscardRequest {
    AdtInactiveSourceDiscardRequest {
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
                .insert_header("x-csrf-token", "discard-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=discard-test; Path=/"),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_lock(server: &MockServer) {
    mount_lock_with_transport(server, None).await;
}

async fn mount_lock_with_transport(server: &MockServer, transport: Option<&str>) {
    let mut mock = Mock::given(method("POST"))
        .and(path(OBJECT_URI))
        .and(query_param("_action", "LOCK"))
        .and(query_param("accessMode", "MODIFY"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "discard-csrf"))
        .and(header("cookie", "SAP_SESSIONID=discard-test"))
        .and(header("x-sap-adt-sessiontype", "stateful"));
    mock = if let Some(transport) = transport {
        mock.and(query_param("corrNr", transport))
    } else {
        mock.and(query_param_is_missing("corrNr"))
    };
    mock.respond_with(ResponseTemplate::new(200).set_body_string(format!(
        "<lockResult><LOCK_HANDLE>{LOCK_HANDLE}</LOCK_HANDLE></lockResult>"
    )))
    .expect(1)
    .mount(server)
    .await;
}

async fn mount_unlock(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(OBJECT_URI))
        .and(query_param("_action", "UNLOCK"))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(query_param("sap-client", "100"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

#[derive(Clone)]
struct InactiveObjectSequence {
    calls: Arc<AtomicUsize>,
    visible_calls: usize,
}

impl Respond for InactiveObjectSequence {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let body = if call < self.visible_calls {
            format!("<objects><object uri=\"{OBJECT_URI}\"/></objects>")
        } else {
            "<objects/>".to_owned()
        };
        ResponseTemplate::new(200).set_body_string(body)
    }
}

async fn mount_inactive_sequence(server: &MockServer, visible_calls: usize, calls: u64) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .and(query_param("sap-client", "100"))
        .respond_with(InactiveObjectSequence {
            calls: Arc::new(AtomicUsize::new(0)),
            visible_calls,
        })
        .expect(calls)
        .mount(server)
        .await;
}

#[derive(Clone)]
struct LockedInactiveSourceSequence {
    calls: Arc<AtomicUsize>,
    restored_source: &'static str,
}

impl Respond for LockedInactiveSourceSequence {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let source = if call == 0 {
            INACTIVE_SOURCE
        } else {
            self.restored_source
        };
        ResponseTemplate::new(200).set_body_string(source)
    }
}

async fn mount_locked_source_reads(server: &MockServer, restored_source: &'static str) {
    Mock::given(method("GET"))
        .and(path(SOURCE_URI))
        .and(query_param("version", "active"))
        .and(query_param("sap-client", "100"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ACTIVE_SOURCE))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(SOURCE_URI))
        .and(query_param("version", "inactive"))
        .and(query_param("sap-client", "100"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(LockedInactiveSourceSequence {
            calls: Arc::new(AtomicUsize::new(0)),
            restored_source,
        })
        .expect(2)
        .mount(server)
        .await;
}

async fn mount_initial_locked_source_reads(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(SOURCE_URI))
        .and(query_param("version", "active"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ACTIVE_SOURCE))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(SOURCE_URI))
        .and(query_param("version", "inactive"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(ResponseTemplate::new(200).set_body_string(INACTIVE_SOURCE))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_restore_write(server: &MockServer, response: ResponseTemplate) {
    mount_restore_write_with_transport(server, None, response).await;
}

async fn mount_restore_write_with_transport(
    server: &MockServer,
    transport: Option<&str>,
    response: ResponseTemplate,
) {
    let mut mock = Mock::given(method("PUT"))
        .and(path(SOURCE_URI))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(query_param("sap-client", "100"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .and(header("content-type", "text/plain; charset=utf-8"))
        .and(body_string(ACTIVE_SOURCE));
    mock = if let Some(transport) = transport {
        mock.and(query_param("corrNr", transport))
    } else {
        mock.and(query_param_is_missing("corrNr"))
    };
    mock.respond_with(response).expect(1).mount(server).await;
}

fn checkrun_body() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><chkrun:checkObjectList xmlns:chkrun=\"http://www.sap.com/adt/checkrun\" xmlns:adtcore=\"http://www.sap.com/adt/core\"><chkrun:checkObject adtcore:uri=\"{SOURCE_URI}\" chkrun:version=\"inactive\"/></chkrun:checkObjectList>"
    )
}

async fn mount_activation_precheck(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(SOURCE_URI))
        .and(query_param("version", "inactive"))
        .and(query_param("sap-client", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ACTIVE_SOURCE))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/checkruns"))
        .and(query_param("reporters", "abapCheckRun"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "discard-csrf"))
        .and(header("cookie", "SAP_SESSIONID=discard-test"))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.checkobjects+xml",
        ))
        .and(body_string(checkrun_body()))
        .respond_with(ResponseTemplate::new(200).set_body_string("<checkMessageList/>"))
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
        .and(header("content-type", "application/xml"))
        .and(body_string(activation_body()))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_post_activation_read(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(SOURCE_URI))
        .and(query_param("version", "active"))
        .and(query_param("sap-client", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ACTIVE_SOURCE))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_successful_discard(server: &MockServer) {
    mount_csrf_session(server).await;
    mount_lock(server).await;
    mount_inactive_sequence(server, 2, 3).await;
    mount_locked_source_reads(server, ACTIVE_SOURCE).await;
    mount_restore_write(server, ResponseTemplate::new(200)).await;
    mount_unlock(server, ResponseTemplate::new(200)).await;
    mount_activation_precheck(server).await;
    mount_activation(
        server,
        ResponseTemplate::new(200)
            .set_body_string(r#"<activationResult activationExecuted="true"/>"#),
    )
    .await;
    mount_post_activation_read(server).await;
}

#[tokio::test]
async fn restores_active_source_under_lock_then_activates_and_verifies_it() {
    let server = MockServer::start().await;
    mount_successful_discard(&server).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result =
        discard_inactive_adt_source(&mut client, &["Z*".to_owned()], &discard_request(None))
            .await
            .unwrap();

    assert_eq!(result.identity.name, "ZCL_SAMPLE");
    assert_eq!(result.discarded.sha256, source_sha256(INACTIVE_SOURCE));
    assert_eq!(result.active_before.sha256, source_sha256(ACTIVE_SOURCE));
    assert_eq!(result.restored_inactive.sha256, result.active_before.sha256);
    assert_eq!(result.active_after.sha256, result.active_before.sha256);
    assert!(result.activation_response_parsed);
    assert_eq!(result.sap_reported_activation_executed, Some(true));

    let requests = server.received_requests().await.unwrap();
    let lock = request_position(&requests, OBJECT_URI, "_action", "LOCK");
    let write = requests
        .iter()
        .position(|request| request.method.as_str() == "PUT")
        .unwrap();
    let unlock = request_position(&requests, OBJECT_URI, "_action", "UNLOCK");
    let activation = request_position(&requests, "/sap/bc/adt/activation", "method", "activate");
    assert!(lock < write);
    assert!(write < unlock);
    assert!(unlock < activation);
    server.verify().await;
}

#[tokio::test]
async fn no_inactive_version_is_detected_after_lock_and_still_unlocks() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server).await;
    mount_inactive_sequence(&server, 0, 1).await;
    mount_unlock(&server, ResponseTemplate::new(200)).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error =
        discard_inactive_adt_source(&mut client, &["Z*".to_owned()], &discard_request(None))
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtInactiveSourceDiscardError::NoInactiveVersion { .. }
    ));
    let requests = server.received_requests().await.unwrap();
    assert!(
        request_position(&requests, OBJECT_URI, "_action", "LOCK")
            < request_position(&requests, OBJECT_URI, "_action", "UNLOCK")
    );
    server.verify().await;
}

#[tokio::test]
async fn uses_transport_for_restore_without_relocking_during_activation() {
    const TRANSPORT: &str = "DE3K900575";
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock_with_transport(&server, Some(TRANSPORT)).await;
    mount_inactive_sequence(&server, 2, 3).await;
    mount_locked_source_reads(&server, ACTIVE_SOURCE).await;
    mount_restore_write_with_transport(&server, Some(TRANSPORT), ResponseTemplate::new(200)).await;
    mount_unlock(&server, ResponseTemplate::new(200)).await;
    mount_activation_precheck(&server).await;
    mount_activation(
        &server,
        ResponseTemplate::new(200)
            .set_body_string(r#"<activationResult activationExecuted="true"/>"#),
    )
    .await;
    mount_post_activation_read(&server).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = discard_inactive_adt_source(
        &mut client,
        &["Z*".to_owned()],
        &discard_request(Some(TRANSPORT)),
    )
    .await
    .unwrap();

    assert_eq!(result.transport.as_deref(), Some(TRANSPORT));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                request.url.path() == OBJECT_URI
                    && request
                        .url
                        .query_pairs()
                        .any(|(key, value)| key == "_action" && value == "LOCK")
            })
            .count(),
        1
    );
    server.verify().await;
}

#[tokio::test]
async fn restore_write_failure_wins_but_unlock_is_still_attempted() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server).await;
    mount_inactive_sequence(&server, 1, 1).await;
    mount_initial_locked_source_reads(&server).await;
    mount_restore_write(
        &server,
        ResponseTemplate::new(409)
            .set_body_string("<error><message>Source is busy</message></error>"),
    )
    .await;
    mount_unlock(&server, ResponseTemplate::new(500)).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error =
        discard_inactive_adt_source(&mut client, &["Z*".to_owned()], &discard_request(None))
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtInactiveSourceDiscardError::Session(AdtEditSessionError::SourceWriteFailed { .. })
    ));
    assert_eq!(error.code(), "edit_discard_restore_write_failed");
    assert!(error.to_string().contains("Source is busy"));
    server.verify().await;
}

#[tokio::test]
async fn normalization_mismatch_stops_before_unlock_and_activation() {
    const NORMALIZED_SOURCE: &str = "CLASS zcl_sample DEFINITION.\r\nENDCLASS.\r\n";
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server).await;
    mount_inactive_sequence(&server, 1, 1).await;
    mount_locked_source_reads(&server, NORMALIZED_SOURCE).await;
    mount_restore_write(&server, ResponseTemplate::new(200)).await;
    mount_unlock(&server, ResponseTemplate::new(200)).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error =
        discard_inactive_adt_source(&mut client, &["Z*".to_owned()], &discard_request(None))
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtInactiveSourceDiscardError::RestoreVerificationMismatch { .. }
    ));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| {
                !(request.url.path() == "/sap/bc/adt/activation"
                    && request
                        .url
                        .query()
                        .is_some_and(|query| query.contains("method=activate")))
            })
    );
    server.verify().await;
}

#[tokio::test]
async fn activation_failure_is_reported_as_an_incomplete_restored_discard() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server).await;
    mount_inactive_sequence(&server, 2, 2).await;
    mount_locked_source_reads(&server, ACTIVE_SOURCE).await;
    mount_restore_write(&server, ResponseTemplate::new(200)).await;
    mount_unlock(&server, ResponseTemplate::new(200)).await;
    mount_activation_precheck(&server).await;
    mount_activation(
        &server,
        ResponseTemplate::new(409).set_body_string("<error>Activation refused</error>"),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error =
        discard_inactive_adt_source(&mut client, &["Z*".to_owned()], &discard_request(None))
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtInactiveSourceDiscardError::RestoredSourceActivation(
            AdtSourceActivationError::ActivationRequest(_)
        )
    ));
    assert!(error.hint().contains("now contains"));
    server.verify().await;
}

#[tokio::test]
async fn validates_transport_and_namespace_before_any_http_request() {
    let server = MockServer::start().await;
    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let invalid_transport = discard_inactive_adt_source(
        &mut client,
        &["Z*".to_owned()],
        &discard_request(Some("not valid!")),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        invalid_transport,
        AdtInactiveSourceDiscardError::Validation(AdtEditTargetValidationError::InvalidTransport(
            _
        ))
    ));

    let namespace =
        discard_inactive_adt_source(&mut client, &["Y*".to_owned()], &discard_request(None))
            .await
            .unwrap_err();
    assert!(matches!(
        namespace,
        AdtInactiveSourceDiscardError::Validation(AdtEditTargetValidationError::Namespace(_))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
}

fn request_position(requests: &[Request], request_path: &str, key: &str, value: &str) -> usize {
    requests
        .iter()
        .position(|request| {
            request.url.path() == request_path
                && request
                    .url
                    .query_pairs()
                    .any(|(candidate_key, candidate_value)| {
                        candidate_key == key && candidate_value == value
                    })
        })
        .unwrap()
}
