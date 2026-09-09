//! Activating a metadata object.
//!
//! The assertion that matters most is the refusal. SAP has been observed
//! answering `activationExecuted="true"` for an activation that plainly failed,
//! so this path decides on the object's own post-state and never on the flag.
//! A test pins exactly that response.

mod adt_edit_mock;

use adt_edit_mock::AdtEditSession;
use fractal::config::{EditPolicy, Profile};
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    client::SapClient,
    metadata_activation::{MetadataObjectActivationRequest, activate_metadata_object},
    metadata_object::MetadataAdtObjectType,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path, query_param},
};

const OBJECT_PATH: &str = "/sap/bc/adt/ddic/dataelements/zsample_de";
const ACTIVATION_PATH: &str = "/sap/bc/adt/activation";
const INACTIVE_PATH: &str = "/sap/bc/adt/activation/inactiveobjects";

fn session() -> AdtEditSession {
    AdtEditSession {
        sap_client: "100",
        csrf_token: "metadata-activation-token",
        session_cookie: "SAP_SESSIONID=metadata-activation",
        object_path: OBJECT_PATH,
        source_path: "",
        lock_handle: "metadata-activation-lock",
    }
}

fn profile(base_url: String) -> Profile {
    session().profile(base_url, &["Z*"])
}

fn request() -> MetadataObjectActivationRequest {
    MetadataObjectActivationRequest {
        object_type: MetadataAdtObjectType::DataElement,
        name: "zsample_de".to_owned(),
        transport: None,
    }
}

fn document(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core"
    adtcore:name="ZSAMPLE_DE" adtcore:type="DTEL/DE" adtcore:version="{version}"
    adtcore:description="Sample"/>"#
    )
}

fn inactive_list(listed: bool) -> String {
    if listed {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects" xmlns:adtcore="http://www.sap.com/adt/core">
  <ioc:entry><ioc:object><ioc:ref adtcore:uri="{OBJECT_PATH}" adtcore:name="ZSAMPLE_DE"/></ioc:object></ioc:entry>
</ioc:inactiveObjects>"#
        )
    } else {
        r#"<?xml version="1.0" encoding="utf-8"?>
<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"/>"#
            .to_owned()
    }
}

/// The list is probed twice, and the two answers mean different things: before
/// the activation it says there is something to activate, after it says whether
/// the activation actually took it.
async fn mount_inactive_list_before_and_after(server: &MockServer, before: bool, after: bool) {
    Mock::given(method("GET"))
        .and(path(INACTIVE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(inactive_list(before)))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(INACTIVE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(inactive_list(after)))
        .mount(server)
        .await;
}

/// A single probe, for the paths that never reach the activation.
async fn mount_inactive_list(server: &MockServer, listed: bool) {
    Mock::given(method("GET"))
        .and(path(INACTIVE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(inactive_list(listed)))
        .mount(server)
        .await;
}

/// The verification read, which must ask for the active version explicitly:
/// a plain GET serves the inactive document whenever one exists.
async fn mount_active_read(server: &MockServer, version: &str, times: u64) {
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_string(document(version)))
        .expect(times)
        .mount(server)
        .await;
}

async fn mount_activation(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path(ACTIVATION_PATH))
        .and(query_param("method", "activate"))
        .and(body_string_contains("adtcore:objectReference"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_owned()))
        .expect(1)
        .mount(server)
        .await;
}

fn succeeded() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist">
  <chkl:properties checkExecuted="true" activationExecuted="true" generationExecuted="false"/>
</chkl:messages>"#
}

/// Exactly what SAP returned for a data element pointing at a missing domain.
fn failed_but_claims_executed() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist">
  <chkl:properties checkExecuted="true" activationExecuted="true" generationExecuted="false"/>
  <msg objDescr="" type="E" line="0"><shortText><txt>Activation was cancelled.</txt></shortText></msg>
  <msg objDescr="DTEL ZSAMPLE_DE" type="E" line="1"><shortText><txt>No active domain ZSAMPLE_DOM available</txt></shortText></msg>
</chkl:messages>"#
}

async fn client(server: &MockServer) -> SapClient {
    SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap()
}

#[tokio::test]
async fn activates_a_data_element_and_proves_it_is_active() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    mount_inactive_list_before_and_after(&server, true, false).await;
    mount_activation(&server, succeeded()).await;
    mount_active_read(&server, "active", 1).await;

    let result = activate_metadata_object(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.identity.name, "ZSAMPLE_DE");
    assert!(result.active_xml.contains(r#"adtcore:version="active""#));
    assert_eq!(result.sap_reported_activation_executed, Some(true));
    server.verify().await;
}

#[tokio::test]
async fn a_failed_activation_that_claims_it_executed_is_still_a_failure() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    mount_inactive_list_before_and_after(&server, true, true).await;
    mount_activation(&server, failed_but_claims_executed()).await;
    // The object is still `new`, and still pending. Either alone refuses it.
    mount_active_read(&server, "new", 1).await;

    let error = activate_metadata_object(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_activate_refused");
    // Taking SAP's flag at face value would have reported this as success.
    let hint = error.hint().expect("has a hint");
    assert!(hint.contains("No active domain"), "{hint}");
    server.verify().await;
}

#[tokio::test]
async fn an_object_read_back_as_inactive_is_not_reported_as_activated() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    // It left the list, so only the document's own layer catches this one.
    mount_inactive_list_before_and_after(&server, true, false).await;
    mount_activation(&server, succeeded()).await;
    mount_active_read(&server, "inactive", 1).await;

    let error = activate_metadata_object(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_activate_refused");
    server.verify().await;
}

#[tokio::test]
async fn an_object_with_no_pending_changes_is_never_posted() {
    let server = MockServer::start().await;
    // Deliberately no CSRF session mock: the refusal happens before one is
    // established, so mounting it would go unused.
    mount_inactive_list(&server, false).await;
    Mock::given(method("POST"))
        .and(path(ACTIVATION_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let error = activate_metadata_object(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_activate_no_inactive_version");
    // The probe answers for the calling user, so the wording has to say so.
    assert!(
        error
            .hint()
            .unwrap()
            .starts_with("You have no pending changes")
    );
    server.verify().await;
}

#[tokio::test]
async fn a_name_outside_the_customer_namespaces_never_reaches_sap() {
    let server = MockServer::start().await;

    let error = activate_metadata_object(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectActivationRequest {
            name: "sapsample_de".to_owned(),
            ..request()
        },
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_customer_namespaces");
    server.verify().await;
}

#[tokio::test]
async fn a_package_outside_the_allowlist_is_refused_before_activating() {
    let server = MockServer::start().await;
    // The allowlist guard reads the object to find its package.
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<blue:wbobj xmlns:blue="urn:b" xmlns:adtcore="urn:a"><adtcore:packageRef adtcore:name="ZOTHER"/></blue:wbobj>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(ACTIVATION_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let error = activate_metadata_object(
        &mut client(&server).await,
        &EditPolicy {
            customer_namespaces: vec!["Z*".to_owned()],
            edit_packages: Some(vec!["ZPROJ*".to_owned()]),
            allow_temporary_package: true,
        },
        &request(),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_edit_packages");
    server.verify().await;
}

#[tokio::test]
async fn a_failed_activation_on_an_already_active_object_is_not_success() {
    // The trap: the object already had an active version from an earlier
    // activation. This one fails, so the active read returns that *older*
    // document — still reporting `version="active"`. The version attribute
    // alone cannot tell "I just activated this" from "it was already active",
    // so the object must also have left the inactive list.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    // Still pending afterwards is the only signal that catches this one.
    mount_inactive_list_before_and_after(&server, true, true).await;
    mount_activation(&server, failed_but_claims_executed()).await;
    mount_active_read(&server, "active", 1).await;

    let error = activate_metadata_object(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_activate_refused");
    server.verify().await;
}
