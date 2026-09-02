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
use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        metadata_object::{MetadataAdtObjectType, write_metadata_object},
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
<blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements" adtcore:name="ZSAMPLE_DE" adtcore:description="Sample">
  <dtel:dataElement><dtel:shortFieldLabel>{label}</dtel:shortFieldLabel></dtel:dataElement>
</blue:wbobj>"#
    )
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
        &["Z*".to_owned()],
        MetadataAdtObjectType::DataElement,
        "zsample_de",
        xml,
        None,
    )
    .await
    .expect_err("expected the write to fail")
}

async fn write(server: &MockServer, xml: &str) -> Result<String, String> {
    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    write_metadata_object(
        &mut client,
        &["Z*".to_owned()],
        MetadataAdtObjectType::DataElement,
        "zsample_de",
        xml,
        None,
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
    mount_reads(&server, &document("old"), &document("new")).await;

    let outcome = write(&server, &document("new")).await.unwrap();

    assert!(outcome.starts_with("true:"), "expected a changed document");
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
    mount_reads(&server, &document("same"), &document("same")).await;

    let outcome = write(&server, &document("same")).await.unwrap();

    assert!(
        outcome.starts_with("false:"),
        "expected an unchanged document"
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
        .expect(1)
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
        .expect(1)
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
        &["Z*".to_owned()],
        MetadataAdtObjectType::DataElement,
        "SFLIGHT_DE",
        &document("new"),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_customer_namespaces");
    assert!(server.received_requests().await.unwrap().is_empty());
}
