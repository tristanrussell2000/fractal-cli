//! Deletion: where-used first, then lock, delete, and prove it is gone.
//!
//! The central assertions here are refusals. A guarded delete that quietly
//! proceeds, or a delete that reports success while the object is still
//! readable, are the two failures that would actually hurt someone.

use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        editable_source::EditableAdtObjectType,
        object_deletion::{
            AdtObjectDeletionError, AdtObjectDeletionRequest, delete_adt_object,
            preview_adt_object_deletion,
        },
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param, query_param_is_missing},
};

const OBJECT_PATH: &str = "/sap/bc/adt/programs/programs/zsample";
const USAGES_PATH: &str = "/sap/bc/adt/repository/informationsystem/usageReferences";
const LOCK_HANDLE: &str = "202608311234567890";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "903".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

fn deletion_request(force: bool, transport: Option<&str>) -> AdtObjectDeletionRequest {
    AdtObjectDeletionRequest {
        object_type: EditableAdtObjectType::Program,
        name: "zsample".to_owned(),
        transport: transport.map(str::to_owned),
        force,
    }
}

async fn mount_csrf_session(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "delete-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=delete-test; Path=/"),
        )
        .mount(server)
        .await;
}

/// `isResult="true"` marks a genuine reference; the other row is breadcrumb
/// context, which must not block a delete.
fn usages_with_one_direct_reference() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
    <usageReferences:usageReferenceResult xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences" xmlns:adtcore="http://www.sap.com/adt/core">
      <usageReferences:referencedObjects>
        <usageReferences:referencedObject uri="/sap/bc/adt/oo/classes/zcl_caller" isResult="true" parentUri="">
          <usageReferences:adtObject adtcore:name="ZCL_CALLER" adtcore:type="CLAS/OC"/>
        </usageReferences:referencedObject>
        <usageReferences:referencedObject uri="/sap/bc/adt/packages/zpkg" isResult="false" parentUri="">
          <usageReferences:adtObject adtcore:name="ZPKG" adtcore:type="DEVC/K"/>
        </usageReferences:referencedObject>
      </usageReferences:referencedObjects>
    </usageReferences:usageReferenceResult>"#
}

fn no_usages() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
    <usageReferences:usageReferenceResult xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences"><usageReferences:referencedObjects/></usageReferences:usageReferenceResult>"#
}

async fn mount_usages(server: &MockServer, body: &'static str) {
    Mock::given(method("POST"))
        .and(path(USAGES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_lock(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "LOCK"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<lockResult><LOCK_HANDLE>{LOCK_HANDLE}</LOCK_HANDLE></lockResult>"
        )))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn deletes_an_unreferenced_object_and_proves_it_is_gone() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, no_usages()).await;
    mount_lock(&server).await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(query_param("corrNr", "DE3K900575"))
        .and(header("x-sap-adt-sessiontype", "stateful"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_string("<error><message>Not found</message></error>"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = delete_adt_object(
        &mut client,
        &["Z*".to_owned()],
        &deletion_request(false, Some("DE3K900575")),
    )
    .await
    .unwrap();

    assert_eq!(result.identity.name, "ZSAMPLE");
    assert!(result.direct_usages.is_empty());
    assert!(!result.forced);
    server.verify().await;
}

#[tokio::test]
async fn refuses_a_referenced_object_without_locking_or_deleting() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, usages_with_one_direct_reference()).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = delete_adt_object(
        &mut client,
        &["Z*".to_owned()],
        &deletion_request(false, None),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, AdtObjectDeletionError::ObjectInUse { .. }));
    assert_eq!(error.code(), "edit_delete_object_in_use");
    assert!(error.hint().unwrap().contains("ZCL_CALLER"));
    assert!(error.hint().unwrap().contains("--force"));
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal object usages /sap/bc/adt/programs/programs/zsample --direct-results")
    );

    // Nothing beyond discovery and the where-used lookup may have happened.
    let requests = server.received_requests().await.unwrap();
    for request in &requests {
        assert_ne!(request.method, wiremock::http::Method::DELETE);
        assert!(request.url.query_pairs().all(|(key, _)| key != "_action"));
    }
    assert_eq!(requests.len(), 2);
}

#[tokio::test]
async fn force_overrides_the_reference_guard_and_records_what_was_overridden() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, usages_with_one_direct_reference()).await;
    mount_lock(&server).await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .and(query_param_is_missing("corrNr"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(404).set_body_string("<error/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = delete_adt_object(
        &mut client,
        &["Z*".to_owned()],
        &deletion_request(true, None),
    )
    .await
    .unwrap();

    assert!(result.forced);
    assert_eq!(result.direct_usages, vec!["ZCL_CALLER".to_owned()]);
    server.verify().await;
}

#[tokio::test]
async fn a_delete_that_leaves_the_object_readable_is_not_success() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, no_usages()).await;
    mount_lock(&server).await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // SAP said 200 but the object is still there.
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<program:abapProgram/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = delete_adt_object(
        &mut client,
        &["Z*".to_owned()],
        &deletion_request(false, None),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, AdtObjectDeletionError::NotDeleted { .. }));
    assert_eq!(error.code(), "edit_delete_not_verified");
    assert!(error.hint().unwrap().contains("Do not retry blindly"));
    server.verify().await;
}

#[tokio::test]
async fn a_failed_delete_releases_the_lock_it_took() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, no_usages()).await;
    mount_lock(&server).await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_string("<error><message>Not authorized to delete</message></error>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "UNLOCK"))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = delete_adt_object(
        &mut client,
        &["Z*".to_owned()],
        &deletion_request(false, None),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_delete_request_failed");
    assert_eq!(error.status(), Some(403));
    assert!(matches!(
        error,
        AdtObjectDeletionError::DeleteRequest { .. }
    ));
    // The unlock mock's `expect(1)` is the assertion: the object still exists,
    // so its lock had to be released.
    server.verify().await;
}

#[tokio::test]
async fn a_delete_that_fails_and_cannot_unlock_reports_both() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, no_usages()).await;
    mount_lock(&server).await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_string("<error><message>Not authorized to delete</message></error>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "UNLOCK"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_string("<error><message>Lock server unavailable</message></error>"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = delete_adt_object(
        &mut client,
        &["Z*".to_owned()],
        &deletion_request(false, None),
    )
    .await
    .unwrap_err();

    // The delete failure stays the reported cause: it is why nothing happened.
    assert_eq!(error.code(), "edit_delete_request_failed");
    assert_eq!(error.status(), Some(403));
    assert!(error.message().contains("Not authorized"));
    assert!(matches!(error, AdtObjectDeletionError::AbandonedLock(_)));
    // ...but the caller is told the object is stuck, because that changes what
    // they have to do next.
    assert!(error.hint().unwrap().contains("still locked"));
    server.verify().await;
}

#[tokio::test]
async fn a_dry_run_reports_the_refusal_without_locking() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, usages_with_one_direct_reference()).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let preview = preview_adt_object_deletion(
        &mut client,
        &["Z*".to_owned()],
        &deletion_request(false, None),
    )
    .await
    .unwrap();

    assert!(!preview.would_delete);
    assert_eq!(preview.direct_usages, vec!["ZCL_CALLER".to_owned()]);

    let requests = server.received_requests().await.unwrap();
    for request in &requests {
        assert_ne!(request.method, wiremock::http::Method::DELETE);
        assert!(request.url.query_pairs().all(|(key, _)| key != "_action"));
    }
}

#[tokio::test]
async fn a_dry_run_with_force_reports_that_it_would_proceed() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, usages_with_one_direct_reference()).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let preview = preview_adt_object_deletion(
        &mut client,
        &["Z*".to_owned()],
        &deletion_request(true, None),
    )
    .await
    .unwrap();

    assert!(preview.would_delete);
    assert_eq!(preview.direct_usages, vec!["ZCL_CALLER".to_owned()]);
}

#[tokio::test]
async fn refuses_an_object_outside_the_customer_namespaces_before_any_request() {
    let server = MockServer::start().await;
    let request = AdtObjectDeletionRequest {
        name: "SAP_STANDARD".to_owned(),
        ..deletion_request(true, None)
    };

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = delete_adt_object(&mut client, &["Z*".to_owned()], &request)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "object_outside_customer_namespaces");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "--force must not skip the namespace guard"
    );
}
