mod adt_edit_mock;

use adt_edit_mock::{AdtEditSession, SequentialResponses};
use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        edit_session::AdtEditSessionError,
        editable_source::{AdtEditTargetValidationError, EditableAdtObjectType},
        source_patch::{
            AdtSourcePatchError, AdtSourcePatchRequest, patch_adt_source_atomically,
            preview_adt_source_patch,
        },
    },
    source_change::{SourceChangePlanError, source_sha256},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string, header, method, path, query_param, query_param_is_missing},
};

const SESSION: AdtEditSession = AdtEditSession {
    sap_client: "903",
    csrf_token: "mock-csrf-token",
    session_cookie: "SAP_SESSIONID=edit-test",
    object_path: "/sap/bc/adt/programs/programs/zsample",
    source_path: "/sap/bc/adt/programs/programs/zsample/source/main",
    lock_handle: "202608211234567890",
};

const OBJECT_PATH: &str = "/sap/bc/adt/programs/programs/zsample";
const SOURCE_PATH: &str = "/sap/bc/adt/programs/programs/zsample/source/main";
const LOCK_HANDLE: &str = "202608211234567890";
const ORIGINAL_SOURCE: &str = "REPORT zsample.\nWRITE 'before'.\n";
const PROPOSED_SOURCE: &str = "REPORT zsample.\nWRITE 'after'.\n";

fn profile(base_url: String) -> Profile {
    SESSION.profile(base_url, &["Z*", "/ACME/*"])
}

fn patch_request() -> AdtSourcePatchRequest {
    AdtSourcePatchRequest {
        object_type: EditableAdtObjectType::Program,
        name: "zsample".to_owned(),
        find: "'before'".to_owned(),
        replace: "'after'".to_owned(),
        expected_sha256: None,
        transport: None,
    }
}

async fn mount_csrf_session(server: &MockServer) {
    SESSION.mount_csrf_session(server).await;
}

async fn mount_successful_lock(server: &MockServer) {
    SESSION.mount_lock(server, None).await;
}

async fn mount_successful_transport_lock(server: &MockServer, transport: &'static str) {
    SESSION.mount_lock(server, Some(transport)).await;
}

async fn mount_unlock(server: &MockServer, status: u16) {
    SESSION
        .mount_unlock(
            server,
            ResponseTemplate::new(status)
                .set_body_string("<error><message>mock unlock response</message></error>"),
        )
        .await;
}

/// The patched-source PUT under a transport. The exact body is this suite's
/// central assertion: SAP receives the planned source, not the original.
async fn mount_successful_transport_put(server: &MockServer, transport: &'static str) {
    SESSION
        .source_write(Some(transport))
        .and(body_string(PROPOSED_SOURCE))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_source_read(server: &MockServer, source: &'static str) {
    SESSION
        .source_read("inactive")
        .respond_with(ResponseTemplate::new(200).set_body_bytes(source.as_bytes()))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn patches_under_lock_and_reports_the_source_sap_actually_stored() {
    let server = MockServer::start().await;
    let normalized_source = "REPORT zsample.\r\nWRITE 'after'.\r\n";
    mount_csrf_session(&server).await;
    mount_successful_lock(&server).await;
    Mock::given(method("GET"))
        .and(path(SOURCE_PATH))
        .and(query_param("version", "inactive"))
        .respond_with(SequentialResponses::sources(&[
            ORIGINAL_SOURCE,
            normalized_source,
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(SOURCE_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(header("x-csrf-token", "mock-csrf-token"))
        .and(header("cookie", "SAP_SESSIONID=edit-test"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .and(header("content-type", "text/plain; charset=utf-8"))
        .and(body_string(PROPOSED_SOURCE))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    mount_unlock(&server, 200).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result =
        patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &patch_request())
            .await
            .unwrap();

    assert_eq!(result.identity.name, "ZSAMPLE");
    assert_eq!(result.original.sha256, source_sha256(ORIGINAL_SOURCE));
    assert_eq!(result.proposed.sha256, source_sha256(PROPOSED_SOURCE));
    assert_eq!(result.stored.sha256, source_sha256(normalized_source));
    assert_eq!(result.original.bytes, ORIGINAL_SOURCE.len());
    assert_eq!(result.proposed.bytes, PROPOSED_SOURCE.len());
    assert_eq!(result.stored.bytes, normalized_source.len());
    assert_eq!(result.replacements, 1);
    assert_eq!(result.stored.source, normalized_source);

    let requests = server.received_requests().await.unwrap();
    let workflow = requests
        .iter()
        .map(|request| {
            let action = request
                .url
                .query_pairs()
                .find(|(name, _)| name == "_action")
                .map(|(_, value)| value.into_owned());
            (request.method.as_str().to_owned(), action)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        workflow,
        [
            ("GET".to_owned(), None),
            ("POST".to_owned(), Some("LOCK".to_owned())),
            ("GET".to_owned(), None),
            ("PUT".to_owned(), None),
            ("POST".to_owned(), Some("UNLOCK".to_owned())),
            ("GET".to_owned(), None),
        ]
    );
    let source_reads = requests
        .iter()
        .filter(|request| request.url.path() == SOURCE_PATH && request.method.as_str() == "GET")
        .collect::<Vec<_>>();
    assert_eq!(
        source_reads[0]
            .headers
            .get("x-sap-adt-sessiontype")
            .unwrap(),
        "stateful"
    );
    assert!(
        source_reads[1]
            .headers
            .get("x-sap-adt-sessiontype")
            .is_none()
    );
    server.verify().await;
}

#[tokio::test]
async fn previews_a_patch_without_csrf_lock_write_or_unlock_requests() {
    let server = MockServer::start().await;
    mount_source_read(&server, ORIGINAL_SOURCE).await;
    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let mut request = patch_request();
    request.transport = Some(" de3k900575 ".to_owned());

    let preview = preview_adt_source_patch(&client, &profile.customer_namespaces, &request)
        .await
        .unwrap();

    assert_eq!(preview.identity.name, "ZSAMPLE");
    assert_eq!(preview.original.sha256, source_sha256(ORIGINAL_SOURCE));
    assert_eq!(preview.proposed.sha256, source_sha256(PROPOSED_SOURCE));
    assert_eq!(preview.original.bytes, ORIGINAL_SOURCE.len());
    assert_eq!(preview.proposed.bytes, PROPOSED_SOURCE.len());
    assert_eq!(preview.replacements, 1);
    assert_eq!(preview.proposed.source, PROPOSED_SOURCE);
    assert_eq!(preview.transport.as_deref(), Some("DE3K900575"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
    assert_eq!(requests[0].url.path(), SOURCE_PATH);
    assert!(requests[0].headers.get("x-csrf-token").is_none());
    server.verify().await;
}

#[tokio::test]
async fn sends_the_transport_on_both_a_successful_lock_and_source_write() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_successful_transport_lock(&server, "DE3K900575").await;
    Mock::given(method("GET"))
        .and(path(SOURCE_PATH))
        .and(query_param("version", "inactive"))
        .respond_with(SequentialResponses::sources(&[
            ORIGINAL_SOURCE,
            PROPOSED_SOURCE,
        ]))
        .expect(2)
        .mount(&server)
        .await;
    mount_successful_transport_put(&server, "DE3K900575").await;
    mount_unlock(&server, 200).await;
    let mut request = patch_request();
    request.transport = Some("de3k900575".to_owned());

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap();

    assert_eq!(result.transport.as_deref(), Some("DE3K900575"));
    server.verify().await;
}

#[tokio::test]
async fn retries_an_own_request_lock_without_corrnr_but_keeps_corrnr_on_put() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "LOCK"))
        .and(query_param("corrNr", "DE3K900575"))
        .respond_with(ResponseTemplate::new(409).set_body_string(
            "<error><message>Object is already locked in request DE3K900575 of user DEVELOPER</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    mount_successful_lock(&server).await;
    Mock::given(method("GET"))
        .and(path(SOURCE_PATH))
        .and(query_param("version", "inactive"))
        .respond_with(SequentialResponses::sources(&[
            ORIGINAL_SOURCE,
            PROPOSED_SOURCE,
        ]))
        .expect(2)
        .mount(&server)
        .await;
    mount_successful_transport_put(&server, "DE3K900575").await;
    mount_unlock(&server, 200).await;
    let mut request = patch_request();
    request.transport = Some("DE3K900575".to_owned());

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap();

    assert_eq!(result.transport.as_deref(), Some("DE3K900575"));
    server.verify().await;
}

#[tokio::test]
async fn does_not_retry_a_lock_held_by_a_different_transport() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "LOCK"))
        .and(query_param("corrNr", "DE3K900575"))
        .respond_with(ResponseTemplate::new(409).set_body_string(
            "<error><message>Object is already locked in request DE3K900999 of user OTHER</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = patch_request();
    request.transport = Some("DE3K900575".to_owned());

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Session(AdtEditSessionError::LockFailed { .. })
    ));
    assert!(error.hint().contains("DE3K900999"));
    assert!(error.hint().contains("DE3K900575"));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    server.verify().await;
}

#[tokio::test]
async fn rejects_an_invalid_transport_before_any_http_request() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let mut request = patch_request();
    request.transport = Some("DE3 K900575".to_owned());

    let error = patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Validation(AdtEditTargetValidationError::InvalidTransport(_))
    ));
    assert_eq!(error.code(), "invalid_transport_request");
}

#[tokio::test]
async fn suggests_the_request_named_by_sap_when_transport_was_omitted() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_successful_lock(&server).await;
    mount_source_read(&server, ORIGINAL_SOURCE).await;
    Mock::given(method("PUT"))
        .and(path(SOURCE_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(query_param_is_missing("corrNr"))
        .respond_with(ResponseTemplate::new(409).set_body_string(
            "<error><message>Object LIMU METH ZSAMPLE is already locked in request DE3K900575 of user DEVELOPER</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    mount_unlock(&server, 200).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error =
        patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &patch_request())
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Session(AdtEditSessionError::SourceWriteFailed { .. })
    ));
    assert!(error.hint().contains("--transport DE3K900575"));
    server.verify().await;
}

#[tokio::test]
async fn rejects_an_object_outside_customer_namespaces_before_locking() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let mut request = patch_request();
    request.name = "SAP_STANDARD".to_owned();

    let error = patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Validation(AdtEditTargetValidationError::Namespace(_))
    ));
    assert_eq!(error.code(), "object_outside_customer_namespaces");
}

#[tokio::test]
async fn reports_lock_contention_without_reading_or_unlocking() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "LOCK"))
        .respond_with(
            ResponseTemplate::new(409).set_body_string(
                "<error><message>Object is locked by another user</message></error>",
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error =
        patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &patch_request())
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Session(AdtEditSessionError::LockFailed { .. })
    ));
    assert_eq!(error.code(), "edit_lock_failed");
    assert!(error.to_string().contains("locked by another user"));
    server.verify().await;
}

#[tokio::test]
async fn rejects_a_successful_lock_response_that_contains_no_handle() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "LOCK"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<lockResult/>"))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error =
        patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &patch_request())
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Session(AdtEditSessionError::LockHandleMissing { .. })
    ));
    assert_eq!(error.code(), "edit_lock_response_invalid");
    server.verify().await;
}

#[tokio::test]
async fn stale_optional_hash_is_checked_under_lock_and_then_unlocked() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_successful_lock(&server).await;
    mount_source_read(&server, ORIGINAL_SOURCE).await;
    mount_unlock(&server, 200).await;
    let mut request = patch_request();
    request.expected_sha256 = Some(source_sha256("an older source"));

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Patch {
            source: SourceChangePlanError::SourceHashMismatch { .. },
            ..
        }
    ));
    assert_eq!(error.code(), "source_hash_mismatch");
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
async fn missing_patch_anchor_is_checked_under_lock_and_then_unlocked() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_successful_lock(&server).await;
    mount_source_read(&server, ORIGINAL_SOURCE).await;
    mount_unlock(&server, 200).await;
    let mut request = patch_request();
    request.find = "not present".to_owned();

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &request)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Patch {
            source: SourceChangePlanError::AnchorNotFound,
            ..
        }
    ));
    // The pure planner cannot name the object; the patch workflow can, so the
    // hint must carry a command the caller can run as-is.
    assert!(
        error
            .hint()
            .contains("fractal edit read --type PROG --name ZSAMPLE --version inactive"),
        "anchor hint should name a runnable read command: {}",
        error.hint()
    );
    server.verify().await;
}

#[tokio::test]
async fn source_write_error_wins_even_when_cleanup_unlock_also_fails() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_successful_lock(&server).await;
    mount_source_read(&server, ORIGINAL_SOURCE).await;
    Mock::given(method("PUT"))
        .and(path(SOURCE_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string("<error><message>Source syntax rejected</message></error>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_unlock(&server, 500).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error =
        patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &patch_request())
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Session(AdtEditSessionError::SourceWriteFailed { .. })
    ));
    assert_eq!(error.code(), "edit_source_write_failed");
    assert!(error.to_string().contains("Source syntax rejected"));
    server.verify().await;
}

#[tokio::test]
async fn unlock_failure_surfaces_after_a_successful_write() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_successful_lock(&server).await;
    mount_source_read(&server, ORIGINAL_SOURCE).await;
    Mock::given(method("PUT"))
        .and(path(SOURCE_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(body_string(PROPOSED_SOURCE))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    mount_unlock(&server, 500).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error =
        patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &patch_request())
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::Session(AdtEditSessionError::UnlockFailed(_))
    ));
    assert_eq!(error.code(), "edit_unlock_failed");
    server.verify().await;
}

#[tokio::test]
async fn reports_when_a_completed_write_cannot_be_verified() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_successful_lock(&server).await;
    Mock::given(method("GET"))
        .and(path(SOURCE_PATH))
        .and(query_param("version", "inactive"))
        .respond_with(SequentialResponses::new(vec![
            ResponseTemplate::new(200).set_body_bytes(ORIGINAL_SOURCE.as_bytes()),
            ResponseTemplate::new(500)
                .set_body_string("<error><message>Verification unavailable</message></error>"),
        ]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(SOURCE_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(body_string(PROPOSED_SOURCE))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    mount_unlock(&server, 200).await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error =
        patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &patch_request())
            .await
            .unwrap_err();

    assert!(matches!(
        error,
        AdtSourcePatchError::StoredSourceRead { .. }
    ));
    assert_eq!(error.code(), "edit_source_verification_failed");
    assert!(error.to_string().contains("Verification unavailable"));
    assert!(error.hint().contains("write and unlock succeeded"));
    server.verify().await;
}
