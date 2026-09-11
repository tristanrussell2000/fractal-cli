//! Deletion: where-used first, then lock, delete, and prove it is gone.
//!
//! The central assertions here are refusals. A guarded delete that quietly
//! proceeds, or a delete that reports success while the object is still
//! readable, are the two failures that would actually hurt someone.

use fractal::config::EditPolicy;
use fractal::journal::entry::{ContentRef, EntryStatus, EntrySystem, JournalOperation};
use fractal::journal::recorder::Journal;
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
        password_command: None,
        edit_packages: None,
        allow_temporary_package: true,
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
        .and(query_param("corrNr", "AB1K900575"))
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
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, Some("AB1K900575")),
        None,
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
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, None),
        None,
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
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(true, None),
        None,
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
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, None),
        None,
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
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, None),
        None,
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
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, None),
        None,
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
        &EditPolicy::namespaces_only(&["Z*"]),
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
        &EditPolicy::namespaces_only(&["Z*"]),
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
    let error = delete_adt_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request,
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_customer_namespaces");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "--force must not skip the namespace guard"
    );
}

// --- Journaling a delete ---------------------------------------------------
//
// A delete is the one operation SAP offers nothing back from: no version
// history, no inactive layer, nothing. The entry written here is the only copy
// of what the object held, which is why a journal failure stops the delete
// rather than warning about it.

const SOURCE: &str = "REPORT zsample.\nWRITE 'one'.\n";

fn object_xml() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?><program:abapProgram xmlns:program="http://www.sap.com/adt/programs/programs" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE" adtcore:type="PROG/P" adtcore:description="Sample report"><adtcore:packageRef adtcore:name="ZPKG"/></program:abapProgram>"#.to_owned()
}

fn journal(dir: &tempfile::TempDir) -> Journal {
    Journal::with_roots(
        dir.path().join("blobs"),
        dir.path().join("journal/de3"),
        EntrySystem {
            base_url: "https://sap.example:8001".to_owned(),
            profile: "dev".to_owned(),
            client: "903".to_owned(),
            user: "developer".to_owned(),
        },
    )
}

/// The reads a journaled delete adds: the source to keep, and the object's own
/// document for its package and description.
async fn mount_content_reads(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample/source/main"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SOURCE))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(object_xml()))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_journalled_delete_keeps_the_content_and_what_recreating_needs() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, no_usages()).await;
    mount_lock(&server).await;
    mount_content_reads(&server).await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // The read-back that proves it is gone, after the content read above.
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(404).set_body_string("<error/>"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    delete_adt_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, None),
        Some(&journal),
    )
    .await
    .unwrap();

    let entry = journal
        .entries()
        .latest_for(OBJECT_PATH)
        .unwrap()
        .expect("an entry was written");

    assert_eq!(entry.status, EntryStatus::Succeeded);
    // The object is gone, which is not the same as having held nothing.
    assert_eq!(entry.active_after, Some(ContentRef::Absent));
    let before = entry.active_before.sha256().expect("kept the content");
    assert_eq!(journal.blobs().read(before).unwrap(), SOURCE);
    // Package and description come from the object's own document: without
    // them the restore recipe cannot name where to put it back.
    assert_eq!(
        entry.operation,
        JournalOperation::delete(Some("ZPKG".to_owned()), Some("Sample report".to_owned()))
    );

    // Read under the lock, not before it. An unlocked read records what the
    // object looked like a moment before somebody else could have changed it,
    // which is not the same as what the delete destroyed.
    let requests = server.received_requests().await.unwrap();
    let position =
        |predicate: &dyn Fn(&wiremock::Request) -> bool| requests.iter().position(predicate);
    let locked = position(&|request| {
        request
            .url
            .query()
            .is_some_and(|query| query.contains("_action=LOCK"))
    })
    .expect("the lock was taken");
    let read = position(&|request| request.url.path().ends_with("/source/main"))
        .expect("the content was read");
    assert!(locked < read, "the content was read before the lock");

    server.verify().await;
}

#[tokio::test]
async fn a_delete_whose_content_cannot_be_read_never_reaches_sap() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, no_usages()).await;
    mount_lock(&server).await;
    // The source read fails, so there would be no before-image.
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample/source/main"))
        .respond_with(ResponseTemplate::new(500).set_body_string("<error/>"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    // The lock must still come off: a refusal that leaves one behind blocks the
    // next attempt on the lock rather than on the real cause.
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .and(query_param("_action", "UNLOCK"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = delete_adt_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, None),
        Some(&journal),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_delete_content_unreadable");
    assert!(error.hint().unwrap().contains("--no-journal"));
    server.verify().await;
}

#[tokio::test]
async fn without_a_journal_a_delete_reads_no_content_at_all() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, no_usages()).await;
    mount_lock(&server).await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample/source/main"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
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
    delete_adt_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, None),
        None,
    )
    .await
    .unwrap();

    server.verify().await;
}

/// Seals the journal's object directories while answering, so the resolution
/// after the delete cannot write. Deterministic, unlike racing a thread: the
/// DELETE is the one request between the journal's two writes.
#[cfg(unix)]
struct SealOnRequest {
    directory: std::path::PathBuf,
}

#[cfg(unix)]
impl wiremock::Respond for SealOnRequest {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        use std::os::unix::fs::PermissionsExt as _;

        for object in std::fs::read_dir(&self.directory).into_iter().flatten() {
            let path = object.expect("an object directory").path();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500))
                .expect("seal the object directory");
        }
        ResponseTemplate::new(200)
    }
}

#[tokio::test]
#[cfg(unix)]
async fn a_delete_that_landed_is_reported_as_success_even_if_its_entry_cannot_be_resolved() {
    use std::os::unix::fs::PermissionsExt as _;

    // The object is gone. Reporting failure would say otherwise, and the
    // reading a caller is most likely to take from `ok: false` after a delete
    // is that the object still exists.
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_usages(&server, no_usages()).await;
    mount_lock(&server).await;
    mount_content_reads(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entries = journal.entries().root().to_path_buf();
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .respond_with(SealOnRequest {
            directory: entries.clone(),
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(404).set_body_string("<error/>"))
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = delete_adt_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(false, None),
        Some(&journal),
    )
    .await
    .expect("the object is gone, so this must be reported as success");

    assert!(
        result.journal_entry_incomplete.is_some(),
        "a stuck entry must be named rather than dropped"
    );

    for object in std::fs::read_dir(&entries).unwrap() {
        let path = object.unwrap().path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // Degraded but not useless: the content survives at `pending`, which is the
    // whole reason a delete is journaled.
    let entry = journal.entries().list().unwrap().pop().expect("an entry");
    assert_eq!(entry.status, EntryStatus::Pending);
    assert_eq!(
        journal
            .blobs()
            .read(entry.active_before.sha256().unwrap())
            .unwrap(),
        SOURCE
    );
    server.verify().await;
}
