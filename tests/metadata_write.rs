//! Writing a metadata object's XML back under a lock.
//!
//! For this family the XML *is* the object, so a write replaces the whole
//! document. The sequence pinned here is the same discipline the source path
//! uses — lock, write, unlock, read back — with two additions this family
//! forced:
//!
//! - the document SAP returns for a new shell carries **no**
//!   `adtcore:description`, and SAP refuses to save without one, so writing
//!   back exactly what was read fails until the caller adds it. That refusal is
//!   classified rather than surfaced as a bare 400;
//! - a write SAP accepts but does not apply is reported, not hidden.

mod adt_edit_mock;

use adt_edit_mock::AdtEditSession;
use fractal::config::EditPolicy;
use fractal::source_change::source_sha256;
use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        metadata_object::{
            MetadataAdtObjectType, MetadataObjectWriteRequest, write_metadata_object,
        },
        staged_work::StagedEditPolicy,
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

const OBJECT_PATH: &str = "/sap/bc/adt/ddic/dataelements/zsample_de";
const MEDIA_TYPE: &str = "application/vnd.sap.adt.dataelements.v2+xml";
const LOCK_HANDLE: &str = "metadata-write-lock";

fn session() -> AdtEditSession {
    AdtEditSession {
        sap_client: "100",
        csrf_token: "metadata-write-token",
        session_cookie: "SAP_SESSIONID=metadata-write",
        object_path: OBJECT_PATH,
        source_path: "",
        lock_handle: LOCK_HANDLE,
    }
}

fn profile(base_url: String) -> Profile {
    session().profile(base_url, &["Z*"])
}

fn document(label: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:description="Sample">
  <dtel:dataElement><dtel:shortFieldLabel>{label}</dtel:shortFieldLabel></dtel:dataElement>
</blue:wbobj>"#
    )
}

/// The staged-work check the write makes before taking the lock.
///
/// Answers with an **active** document, which is what SAP serves when no
/// inactive version exists, so the guard finds nobody's work staged. Mount it
/// before [`mount_reads`]: all these reads share one URL, so only their order
/// tells them apart. Writes that pass `--expected-sha256` never make this read
/// and must not mount it.
async fn mount_staged_check(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?><blue:wbobj xmlns:blue="urn:b" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:version="active"/>"#,
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
}

/// Answers the two reads in order: before the write, then after it.
async fn mount_reads(server: &MockServer, before: &str, after: &str) {
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(before))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(after))
        .mount(server)
        .await;
}

async fn write_error(
    server: &MockServer,
    xml: &str,
) -> fractal::sap::metadata_object::MetadataObjectWriteError {
    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: xml,
            transport: None,
            expected_sha256: None,
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .expect_err("expected the write to fail")
}

async fn write(server: &MockServer, xml: &str) -> Result<String, String> {
    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: xml,
            transport: None,
            expected_sha256: None,
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .map(|result| format!("{}:{}", result.changed, result.stored_xml.len()))
    .map_err(|error| error.code().to_owned())
}

#[tokio::test]
async fn writes_the_document_under_a_lock_and_reads_back_what_sap_stored() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        // The lock handle is what makes this write legal; without it SAP
        // refuses, and a write outside a lock is the bug this pins.
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(header("content-type", MEDIA_TYPE))
        .and(body_string_contains("<dtel:shortFieldLabel>new</dtel"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    mount_staged_check(&server).await;
    mount_reads(&server, &document("old"), &document("new")).await;

    let outcome = write(&server, &document("new")).await.unwrap();

    assert!(outcome.starts_with("true:"), "expected a changed document");
    server.verify().await;
}

#[tokio::test]
async fn a_table_type_is_written_with_its_own_media_type() {
    // The write PUTs the family's *creation* media type; sending a data
    // element's would be a 415 against a table type's collection.
    let object_path = "/sap/bc/adt/ddic/tabletypes/zsample_tt";
    let server = MockServer::start().await;
    let session = AdtEditSession {
        object_path,
        ..session()
    };
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(object_path))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.tabletype.v1+xml",
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(object_path))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"<ttyp:tableType xmlns:ttyp="urn:t"/>"#),
        )
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(object_path))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<ttyp:tableType xmlns:ttyp=\"urn:t\" edited=\"1\"/>"),
        )
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::TableType,
            name: "zsample_tt",
            xml: "<ttyp:tableType edited=\"1\"/>",
            transport: None,
            expected_sha256: None,
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .unwrap();

    assert_eq!(result.identity.object_type.as_str(), "TTYP");
    assert!(result.changed);
    server.verify().await;
}

#[tokio::test]
async fn a_write_that_changed_nothing_is_reported_rather_than_hidden() {
    // SAP has answered 200 to a request that did nothing before, so "it
    // changed" is checked instead of assumed.
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    mount_staged_check(&server).await;
    mount_reads(&server, &document("same"), &document("same")).await;

    let outcome = write(&server, &document("same")).await.unwrap();

    assert!(
        outcome.starts_with("false:"),
        "expected an unchanged document"
    );
    server.verify().await;
}

#[tokio::test]
async fn a_stuck_lock_after_a_successful_write_is_reported_without_failing_the_write() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(500).set_body_string("<error/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    mount_staged_check(&server).await;
    mount_reads(&server, &document("old"), &document("new")).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: &document("new"),
            transport: None,
            expected_sha256: None,
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .expect("the write landed, so this is not a failure");

    assert!(result.changed);
    assert!(
        result.still_locked,
        "a lock that could not be released must reach the caller"
    );
    server.verify().await;
}

#[tokio::test]
async fn a_refusal_for_a_missing_description_is_classified() {
    // The document `object xml` returns for a new shell has no description,
    // so this is what writing it straight back produces.
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        // The lock still has to come off after a rejected write.
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("<error><message>The description is missing</message></error>"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(document("old")))
        .expect(2)
        .mount(&server)
        .await;

    let code = write(&server, &document("new")).await.unwrap_err();

    assert_eq!(code, "edit_xml_description_missing");
    server.verify().await;
}

#[tokio::test]
async fn a_failed_write_whose_lock_also_stuck_says_the_object_is_still_locked() {
    // The write failure stays the reported cause — a cleanup failure must not
    // mask it — but the stuck lock changes what the caller must do next. Losing
    // it means their retry fails on the lock, with nothing having said why.
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(500).set_body_string("<error/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<error><message>Not authorized to change this object</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(document("old")))
        .expect(2)
        .mount(&server)
        .await;

    let error = write_error(&server, &document("new")).await;

    // The write failure is what is reported, unchanged.
    assert_eq!(error.code(), "edit_xml_write_failed");
    assert_eq!(error.status(), Some(403));
    // And the stuck lock is stated, because it decides the next move.
    let hint = error.hint().unwrap();
    assert!(
        hint.contains("still locked"),
        "hint did not mention the lock: {hint}"
    );
    assert!(hint.contains("clear the lock"));
    server.verify().await;
}

#[tokio::test]
async fn an_empty_document_never_reaches_sap() {
    let server = MockServer::start().await;

    let code = write(&server, "   ").await.unwrap_err();

    assert_eq!(code, "blank_xml_document");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_name_outside_the_customer_namespaces_never_reaches_sap() {
    let server = MockServer::start().await;
    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();

    let error = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "SFLIGHT_DE",
            xml: &document("new"),
            transport: None,
            expected_sha256: None,
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_customer_namespaces");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_matching_expected_hash_lets_the_write_through() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // No staged check: asserting a hash is the caller saying they have seen
    // what is there, so the guard does not read.
    mount_reads(&server, &document("old"), &document("new")).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: &document("new"),
            transport: None,
            expected_sha256: Some(&source_sha256(&document("old"))),
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .unwrap();

    assert!(result.changed);
    server.verify().await;
}

#[tokio::test]
async fn a_stale_expected_hash_refuses_the_write_and_still_releases_the_lock() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    // The lock is taken before the document is read, so a refusal here still
    // has to give it back.
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // No PUT is mounted: an attempted write would fail the test, which is the
    // assertion that matters most here.
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(document("old")))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: &document("new"),
            transport: None,
            // The hash of a document somebody else already replaced.
            expected_sha256: Some(&source_sha256(&document("what the caller last read"))),
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "source_hash_mismatch");
    assert!(
        error.hint().unwrap().contains("Re-read"),
        "the remedy is to re-read and reapply"
    );
    server.verify().await;
}

#[tokio::test]
async fn the_document_is_read_under_the_lock_not_before_it() {
    // A hash checked against an unlocked read proves nothing: the document
    // could change between that read and the lock. Exactly one GET happens on
    // an unrestricted profile that asserts a hash, and it comes after the lock.
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(document("old")))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: &document("new"),
            transport: None,
            expected_sha256: Some("not-a-hash"),
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "invalid_expected_sha256");
    server.verify().await;
}

const NAVIGATION_LINK: &str = r#"<atom:link href="versions" rel="http://www.sap.com/adt/relations/versions" xmlns:atom="http://www.w3.org/2005/Atom"/>"#;

/// The journal stores metadata documents with their `atom:link` navigation
/// stripped, so that is what an undo will hand back to `set-xml`. This pins the
/// two halves of that being safe: the document is sent exactly as given, links
/// and all absent, and a read-back in which SAP has regenerated them is still
/// read back rather than assumed.
///
/// That a real SAP accepts such a document was established live; a mock cannot
/// prove it, and this does not claim to.
#[tokio::test]
async fn a_document_with_its_links_stripped_is_sent_verbatim() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // The read-back has the links SAP regenerated; the document sent did not.
    let stored = document("new").replace(
        "<dtel:dataElement>",
        &format!("{NAVIGATION_LINK}<dtel:dataElement>"),
    );
    mount_staged_check(&server).await;
    mount_reads(&server, &document("old"), &stored).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: &document("new"),
            transport: None,
            expected_sha256: None,
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .unwrap();

    assert!(result.changed);
    assert!(result.stored_xml.contains("atom:link"));
    let sent = &server.received_requests().await.unwrap();
    let put = sent
        .iter()
        .find(|request| request.method == wiremock::http::Method::PUT)
        .expect("the write was sent");
    let body = String::from_utf8(put.body.clone()).unwrap();
    assert!(!body.contains("atom:link"), "{body}");
    assert_eq!(body, document("new"));
    server.verify().await;
}

/// The same document, carrying the package the object lives in.
fn packaged_document(label: &str, package: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:description="Sample">
  <adtcore:packageRef adtcore:name="{package}"/>
  <dtel:dataElement><dtel:shortFieldLabel>{label}</dtel:shortFieldLabel></dtel:dataElement>
</blue:wbobj>"#
    )
}

#[tokio::test]
async fn the_package_guard_reads_the_active_document_not_a_pending_edit() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .and(query_param("lockHandle", LOCK_HANDLE))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // The guard's read, and the only one that names the active layer. A
    // pending edit cannot move an object between packages, and this pins that
    // the guard does not depend on it: the inactive document below claims a
    // package the profile does not grant.
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .and(query_param("version", "active"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(packaged_document("old", "ZGRANTED")),
        )
        .expect(1)
        .mount(&server)
        .await;
    // The gate read and the read-back, both on the layer a write lands in.
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .and(query_param("version", "inactive"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(packaged_document("new", "ZREFUSED")),
        )
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = write_metadata_object(
        &mut client,
        &EditPolicy {
            customer_namespaces: vec!["Z*".to_owned()],
            edit_packages: Some(vec!["ZGRANTED".to_owned()]),
            allow_temporary_package: true,
        },
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: &packaged_document("new", "ZGRANTED"),
            transport: None,
            expected_sha256: None,
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await;

    assert!(result.is_ok(), "{:?}", result.err());
    server.verify().await;
}

#[tokio::test]
async fn a_write_over_another_users_staged_edit_is_refused_before_the_lock() {
    let server = MockServer::start().await;
    let session = session();
    // No CSRF session is mounted either: the refusal lands before anything
    // needs one, which is the cheapest place it can happen.
    // Neither of these may be requested: the refusal is decided before the
    // lock, so it never has one to give back, and nothing is written.
    session
        .lock_request(None)
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?><blue:wbobj xmlns:blue="urn:b" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:version="inactive" adtcore:changedBy="COLLEAGUE"/>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: &document("new"),
            transport: None,
            expected_sha256: None,
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_staged_by_another_user");
    assert!(error.message().contains("COLLEAGUE"), "{}", error.message());
    server.verify().await;
}

#[tokio::test]
async fn the_callers_own_staged_edit_is_not_in_the_way() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // Staged by the caller. Overwriting your own pending edit is ordinary
    // iteration, not a collision.
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?><blue:wbobj xmlns:blue="urn:b" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:version="inactive" adtcore:changedBy="DEVELOPER"/>"#,
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    mount_reads(&server, &document("old"), &document("new")).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = write_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectWriteRequest {
            object_type: MetadataAdtObjectType::DataElement,
            name: "zsample_de",
            xml: &document("new"),
            transport: None,
            expected_sha256: None,
            // SAP reports user ids uppercased; the profile need not match its case.
            staged_edits: &StagedEditPolicy::Protect("developer".to_owned()),
        },
    )
    .await;

    assert!(result.is_ok(), "{:?}", result.err());
    server.verify().await;
}
