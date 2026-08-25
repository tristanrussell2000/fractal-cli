use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use fractal::{
    config::Profile,
    edit::{EditError, source_sha256},
    sap::{
        client::SapClient,
        edit::EditableAdtObjectType,
        source_replace::{
            AdtSourceReplacementError, AdtSourceReplacementRequest, preview_adt_source_replacement,
            replace_adt_source_atomically,
        },
    },
};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{body_string, header, method, path, query_param, query_param_is_missing},
};

const OBJECT_PATH: &str = "/sap/bc/adt/programs/programs/zsample";
const SOURCE_PATH: &str = "/sap/bc/adt/programs/programs/zsample/source/main";
const LOCK_HANDLE: &str = "202608251234567890";
const ORIGINAL_SOURCE: &str = "REPORT zsample.\nWRITE 'before'.\n";
const REPLACEMENT_SOURCE: &str = "REPORT zsample.\nWRITE 'after'.\n";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "903".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

fn replacement_request() -> AdtSourceReplacementRequest {
    AdtSourceReplacementRequest {
        object_type: EditableAdtObjectType::Program,
        name: "zsample".to_owned(),
        replacement_source: REPLACEMENT_SOURCE.to_owned(),
        expected_sha256: None,
        transport: None,
    }
}

async fn mount_csrf_session(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "replace-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=replace-test; Path=/"),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_lock(server: &MockServer, transport: Option<&str>) {
    let mut mock = Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "LOCK"))
        .and(query_param("accessMode", "MODIFY"))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "replace-csrf"))
        .and(header("cookie", "SAP_SESSIONID=replace-test"))
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

async fn mount_unlock(server: &MockServer, status: u16) {
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "UNLOCK"))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(query_param("sap-client", "903"))
        .and(query_param_is_missing("corrNr"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_string("<error><message>mock unlock response</message></error>"),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_write(server: &MockServer, transport: Option<&str>, response: ResponseTemplate) {
    let mut mock = Mock::given(method("PUT"))
        .and(path(SOURCE_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "replace-csrf"))
        .and(header("cookie", "SAP_SESSIONID=replace-test"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .and(header("content-type", "text/plain; charset=utf-8"))
        .and(body_string(REPLACEMENT_SOURCE));
    mock = if let Some(transport) = transport {
        mock.and(query_param("corrNr", transport))
    } else {
        mock.and(query_param_is_missing("corrNr"))
    };
    mock.respond_with(response).expect(1).mount(server).await;
}

async fn mount_single_source_read(server: &MockServer, source: &'static str) {
    mount_single_source_response(server, ResponseTemplate::new(200).set_body_string(source)).await;
}

async fn mount_single_source_response(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(SOURCE_PATH))
        .and(query_param("version", "inactive"))
        .and(query_param("sap-client", "903"))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

#[derive(Clone)]
struct SequentialSourceResponder {
    calls: Arc<AtomicUsize>,
    stored_source: &'static str,
}

impl Respond for SequentialSourceResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let source = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ORIGINAL_SOURCE
        } else {
            self.stored_source
        };
        ResponseTemplate::new(200).set_body_string(source)
    }
}

async fn mount_source_sequence(server: &MockServer, stored_source: &'static str) {
    Mock::given(method("GET"))
        .and(path(SOURCE_PATH))
        .and(query_param("version", "inactive"))
        .and(query_param("sap-client", "903"))
        .respond_with(SequentialSourceResponder {
            calls: Arc::new(AtomicUsize::new(0)),
            stored_source,
        })
        .expect(2)
        .mount(server)
        .await;
}

#[derive(Clone)]
struct FailingVerificationResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for FailingVerificationResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(200).set_body_string(ORIGINAL_SOURCE)
        } else {
            ResponseTemplate::new(500)
                .set_body_string("<error><message>Verification unavailable</message></error>")
        }
    }
}

#[tokio::test]
async fn replaces_complete_source_under_lock_and_reports_sap_normalization() {
    const NORMALIZED_SOURCE: &str = "REPORT zsample.\r\nWRITE 'after'.\r\n";
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, None).await;
    mount_source_sequence(&server, NORMALIZED_SOURCE).await;
    mount_write(&server, None, ResponseTemplate::new(200)).await;
    mount_unlock(&server, 200).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = replace_adt_source_atomically(
        &mut client,
        &profile.customer_namespaces,
        &replacement_request(),
    )
    .await
    .unwrap();

    assert_eq!(result.name, "ZSAMPLE");
    assert_eq!(result.original_sha256, source_sha256(ORIGINAL_SOURCE));
    assert_eq!(result.replacement_sha256, source_sha256(REPLACEMENT_SOURCE));
    assert_eq!(result.stored_sha256, source_sha256(NORMALIZED_SOURCE));
    assert_eq!(result.stored_source, NORMALIZED_SOURCE);
    assert!(result.sap_normalized_source);

    let requests = server.received_requests().await.unwrap();
    let lock = request_position(&requests, OBJECT_PATH, "_action", "LOCK");
    let write = requests
        .iter()
        .position(|request| request.method.as_str() == "PUT")
        .unwrap();
    let unlock = request_position(&requests, OBJECT_PATH, "_action", "UNLOCK");
    let source_reads = requests
        .iter()
        .enumerate()
        .filter(|(_, request)| {
            request.url.path() == SOURCE_PATH && request.method.as_str() == "GET"
        })
        .collect::<Vec<_>>();
    assert_eq!(source_reads.len(), 2);
    assert!(lock < source_reads[0].0);
    assert!(source_reads[0].0 < write);
    assert!(write < unlock);
    assert!(unlock < source_reads[1].0);
    assert_eq!(
        source_reads[0]
            .1
            .headers
            .get("x-sap-adt-sessiontype")
            .unwrap(),
        "stateful"
    );
    assert!(
        source_reads[1]
            .1
            .headers
            .get("x-sap-adt-sessiontype")
            .is_none()
    );
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() != "/sap/bc/adt/activation")
    );
    server.verify().await;
}

#[tokio::test]
async fn previews_complete_source_with_one_read_and_no_mutating_requests() {
    let server = MockServer::start().await;
    mount_single_source_read(&server, ORIGINAL_SOURCE).await;
    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let mut request = replacement_request();
    request.transport = Some(" de3k900575 ".to_owned());

    let preview = preview_adt_source_replacement(&client, &profile.customer_namespaces, &request)
        .await
        .unwrap();

    assert_eq!(preview.original_sha256, source_sha256(ORIGINAL_SOURCE));
    assert_eq!(
        preview.replacement_sha256,
        source_sha256(REPLACEMENT_SOURCE)
    );
    assert_eq!(preview.transport.as_deref(), Some("DE3K900575"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
    server.verify().await;
}

#[tokio::test]
async fn sends_transport_on_the_lock_and_complete_source_write() {
    const TRANSPORT: &str = "DE3K900575";
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, Some(TRANSPORT)).await;
    mount_source_sequence(&server, REPLACEMENT_SOURCE).await;
    mount_write(&server, Some(TRANSPORT), ResponseTemplate::new(200)).await;
    mount_unlock(&server, 200).await;
    let mut request = replacement_request();
    request.transport = Some("de3k900575".to_owned());

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = replace_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap();

    assert_eq!(result.transport.as_deref(), Some(TRANSPORT));
    assert!(!result.sap_normalized_source);
    server.verify().await;
}

#[tokio::test]
async fn stale_expected_hash_is_checked_under_lock_and_then_unlocked() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, None).await;
    mount_single_source_read(&server, ORIGINAL_SOURCE).await;
    mount_unlock(&server, 200).await;
    let mut request = replacement_request();
    request.expected_sha256 = Some(source_sha256("older source"));

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = replace_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceReplacementError::Replacement(EditError::SourceHashMismatch { .. })
    ));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "PUT")
    );
    server.verify().await;
}

#[tokio::test]
async fn unchanged_source_is_rejected_under_lock_without_writing() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, None).await;
    mount_single_source_read(&server, ORIGINAL_SOURCE).await;
    mount_unlock(&server, 200).await;
    let mut request = replacement_request();
    request.replacement_source = ORIGINAL_SOURCE.to_owned();

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = replace_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceReplacementError::Replacement(EditError::SourceReplacementNoChanges)
    ));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "PUT")
    );
    server.verify().await;
}

#[tokio::test]
async fn invalid_expected_hash_format_is_rejected_under_lock_without_writing() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, None).await;
    mount_single_source_read(&server, ORIGINAL_SOURCE).await;
    mount_unlock(&server, 200).await;
    let mut request = replacement_request();
    request.expected_sha256 = Some("not-a-hash".to_owned());

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = replace_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceReplacementError::Replacement(EditError::InvalidExpectedSha256)
    ));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "PUT")
    );
    server.verify().await;
}

#[tokio::test]
async fn failed_locked_source_read_still_unlocks() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, None).await;
    Mock::given(method("GET"))
        .and(path(SOURCE_PATH))
        .and(query_param("version", "inactive"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string("<error><message>Locked read failed</message></error>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_unlock(&server, 200).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = replace_adt_source_atomically(
        &mut client,
        &profile.customer_namespaces,
        &replacement_request(),
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceReplacementError::LockedSourceRead(_)
    ));
    let requests = server.received_requests().await.unwrap();
    let read = requests
        .iter()
        .position(|request| request.url.path() == SOURCE_PATH)
        .unwrap();
    let unlock = request_position(&requests, OBJECT_PATH, "_action", "UNLOCK");
    assert!(read < unlock);
    server.verify().await;
}

#[tokio::test]
async fn failed_preview_source_read_maps_to_preview_error_without_mutation() {
    let server = MockServer::start().await;
    mount_single_source_response(
        &server,
        ResponseTemplate::new(500)
            .set_body_string("<error><message>Preview unavailable</message></error>"),
    )
    .await;
    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();

    let error = preview_adt_source_replacement(
        &client,
        &profile.customer_namespaces,
        &replacement_request(),
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceReplacementError::PreviewSourceRead(_)
    ));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
    server.verify().await;
}

#[tokio::test]
async fn write_failure_wins_even_when_unlock_also_fails() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, None).await;
    mount_single_source_read(&server, ORIGINAL_SOURCE).await;
    mount_write(
        &server,
        None,
        ResponseTemplate::new(500)
            .set_body_string("<error><message>Complete source rejected</message></error>"),
    )
    .await;
    mount_unlock(&server, 500).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = replace_adt_source_atomically(
        &mut client,
        &profile.customer_namespaces,
        &replacement_request(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, AdtSourceReplacementError::SourceWrite(_)));
    assert_eq!(error.code(), "edit_source_replacement_write_failed");
    assert!(error.to_string().contains("Complete source rejected"));
    server.verify().await;
}

#[tokio::test]
async fn successful_write_followed_by_failed_unlock_maps_to_unlock_error() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, None).await;
    mount_single_source_read(&server, ORIGINAL_SOURCE).await;
    mount_write(&server, None, ResponseTemplate::new(200)).await;
    mount_unlock(&server, 500).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = replace_adt_source_atomically(
        &mut client,
        &profile.customer_namespaces,
        &replacement_request(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, AdtSourceReplacementError::Unlock(_)));
    assert_eq!(error.code(), "edit_source_replacement_unlock_failed");
    server.verify().await;
}

#[tokio::test]
async fn failed_post_write_verification_maps_to_stored_source_read_error() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_lock(&server, None).await;
    Mock::given(method("GET"))
        .and(path(SOURCE_PATH))
        .and(query_param("version", "inactive"))
        .respond_with(FailingVerificationResponder {
            calls: Arc::new(AtomicUsize::new(0)),
        })
        .expect(2)
        .mount(&server)
        .await;
    mount_write(&server, None, ResponseTemplate::new(200)).await;
    mount_unlock(&server, 200).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = replace_adt_source_atomically(
        &mut client,
        &profile.customer_namespaces,
        &replacement_request(),
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceReplacementError::StoredSourceRead(_)
    ));
    assert_eq!(error.code(), "edit_source_replacement_verification_failed");
    assert!(error.to_string().contains("Verification unavailable"));
    server.verify().await;
}

#[tokio::test]
async fn validates_blank_source_namespace_and_transport_before_http() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();

    let mut blank = replacement_request();
    blank.replacement_source = " \n\t".to_owned();
    let error = replace_adt_source_atomically(&mut client, &profile.customer_namespaces, &blank)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AdtSourceReplacementError::Replacement(EditError::BlankReplacementSource)
    ));

    let mut invalid_transport = replacement_request();
    invalid_transport.transport = Some("DE3 K900575".to_owned());
    let error = replace_adt_source_atomically(
        &mut client,
        &profile.customer_namespaces,
        &invalid_transport,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        AdtSourceReplacementError::InvalidTransportRequest(_)
    ));

    let mut standard = replacement_request();
    standard.name = "SAP_STANDARD".to_owned();
    let error = replace_adt_source_atomically(&mut client, &profile.customer_namespaces, &standard)
        .await
        .unwrap_err();
    assert!(matches!(error, AdtSourceReplacementError::Namespace(_)));
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
