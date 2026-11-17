//! Creating and deleting packages.
//!
//! The assertions that matter are the requests never sent: a transportable
//! package without a request, and a non-empty package, are refused before
//! anything is created, locked or deleted.

use crate::adt_edit_mock::AdtEditSession;
use fractal::config::{EditPolicy, Profile};
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    client::SapClient,
    package_lifecycle::{PackageCreationRequest, create_package, delete_package},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path, query_param, query_param_is_missing},
};

const PACKAGE_PATH: &str = "/sap/bc/adt/packages/zsample_pkg";
const NODE_STRUCTURE_PATH: &str = "/sap/bc/adt/repository/nodestructure";

fn session() -> AdtEditSession {
    AdtEditSession {
        sap_client: "100",
        csrf_token: "package-token",
        session_cookie: "SAP_SESSIONID=package",
        object_path: PACKAGE_PATH,
        source_path: "",
        lock_handle: "package-lock",
    }
}

fn profile(base_url: String) -> Profile {
    session().profile(base_url, &["Z*"])
}

async fn client(server: &MockServer) -> SapClient {
    SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap()
}

fn policy() -> EditPolicy {
    EditPolicy::namespaces_only(&["Z*"])
}

fn package_document(name: &str) -> String {
    format!(
        r#"<pak:package adtcore:name="{name}" adtcore:description="Sample" xmlns:pak="http://www.sap.com/adt/packages" xmlns:adtcore="http://www.sap.com/adt/core"><pak:superPackage adtcore:name="ZSAMPLE_ROOT"/><pak:transport><pak:softwareComponent pak:name="HOME"/><pak:transportLayer pak:name="ZLAY"/></pak:transport></pak:package>"#
    )
}

fn request(name: &str, transport: Option<&str>, parent: Option<&str>) -> PackageCreationRequest {
    PackageCreationRequest {
        name: name.to_owned(),
        description: "Sample".to_owned(),
        parent: parent.map(str::to_owned),
        software_component: None,
        transport_layer: None,
        transport: transport.map(str::to_owned),
        responsible: "developer".to_owned(),
    }
}

#[tokio::test]
async fn a_subpackage_inherits_its_parents_transport_attributes() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/packages/zsample_root"))
        .respond_with(ResponseTemplate::new(200).set_body_string(package_document("ZSAMPLE_ROOT")))
        .mount(&server)
        .await;
    // Absent before the create, present after it.
    Mock::given(method("GET"))
        .and(path(PACKAGE_PATH))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PACKAGE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(package_document("ZSAMPLE_PKG")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/packages"))
        .and(query_param("corrNr", "A4HK900123"))
        .and(body_string_contains(
            r#"<pak:superPackage adtcore:name="ZSAMPLE_ROOT"/>"#,
        ))
        .and(body_string_contains(
            r#"<pak:softwareComponent pak:name="HOME"/>"#,
        ))
        .and(body_string_contains(
            r#"<pak:transportLayer pak:name="ZLAY"/>"#,
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let result = create_package(
        &mut client(&server).await,
        &policy(),
        &request("zsample_pkg", Some("a4hk900123"), Some("zsample_root")),
    )
    .await
    .unwrap();

    assert_eq!(result.package.name, "ZSAMPLE_PKG");
    assert_eq!(result.transport.as_deref(), Some("A4HK900123"));
    assert!(!result.local);
    server.verify().await;
}

#[tokio::test]
async fn a_local_package_is_created_without_a_request() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    let local = "/sap/bc/adt/packages/%24sample_local";
    Mock::given(method("GET"))
        .and(path(local))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(local))
        .respond_with(ResponseTemplate::new(200).set_body_string(package_document("$SAMPLE_LOCAL")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/packages"))
        .and(query_param_is_missing("corrNr"))
        .and(body_string_contains(
            r#"<pak:softwareComponent pak:name="LOCAL"/>"#,
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let result = create_package(
        &mut client(&server).await,
        &policy(),
        &request("$sample_local", None, None),
    )
    .await
    .unwrap();

    assert!(result.local);
    server.verify().await;
}

#[tokio::test]
async fn a_transportable_package_without_a_request_sends_nothing() {
    let server = MockServer::start().await;

    let error = create_package(
        &mut client(&server).await,
        &policy(),
        &request("zsample_pkg", None, Some("zsample_root")),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "package_transport_required");
    assert!(server.received_requests().await.unwrap().is_empty());
}

fn node_structure_with_one_object() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?><asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA><TREE_CONTENT><SEU_ADT_REPOSITORY_OBJ_NODE><OBJECT_TYPE>DDLS/DF</OBJECT_TYPE><OBJECT_NAME>ZSAMPLE_VIEW</OBJECT_NAME><OBJECT_URI>/sap/bc/adt/ddic/ddl/sources/zsample_view</OBJECT_URI></SEU_ADT_REPOSITORY_OBJ_NODE></TREE_CONTENT></DATA></asx:values></asx:abap>"#
}

#[tokio::test]
async fn a_package_with_contents_is_refused_before_it_is_locked() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("GET"))
        .and(path(PACKAGE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(package_document("ZSAMPLE_PKG")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(NODE_STRUCTURE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(node_structure_with_one_object()))
        .mount(&server)
        .await;
    session()
        .lock_request(None)
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let error = delete_package(
        &mut client(&server).await,
        &policy(),
        "zsample_pkg",
        None,
        false,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "package_not_empty");
    assert!(
        error.message().contains("DDLS/DF ZSAMPLE_VIEW"),
        "{}",
        error.message()
    );
    server.verify().await;
}

#[tokio::test]
async fn deleted_objects_still_registered_to_the_package_are_named_and_the_lock_released() {
    // The package lists as empty, but SAP keeps objects deleted under an
    // unreleased request registered to it and refuses.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("GET"))
        .and(path(PACKAGE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(package_document("ZSAMPLE_PKG")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(NODE_STRUCTURE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .mount(&server)
        .await;
    session().mount_lock(&server, None).await;
    Mock::given(method("DELETE"))
        .and(path(PACKAGE_PATH))
        .and(query_param("lockHandle", "package-lock"))
        .and(query_param("corrNr", "A4HK900123"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            "<exc:exception><message>Package ZSAMPLE_PKG still contains development objects or other packages</message></exc:exception>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    session()
        .mount_unlock(&server, ResponseTemplate::new(200))
        .await;

    let error = delete_package(
        &mut client(&server).await,
        &policy(),
        "zsample_pkg",
        Some("A4HK900123"),
        false,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "package_pending_deletions");
    assert!(error.hint().unwrap().contains("Release that request"));
    server.verify().await;
}

#[tokio::test]
async fn an_empty_answer_is_an_empty_package_only_when_the_package_exists() {
    // SAP answers an empty package and a missing one with the same empty body.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(NODE_STRUCTURE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PACKAGE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(package_document("ZSAMPLE_PKG")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/packages/zsample_missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let mut sap = client(&server).await;

    let empty = fractal::sap::package::get_package_contents(&mut sap, "zsample_pkg")
        .await
        .unwrap();
    assert!(empty.items.is_empty() && empty.subpackages.is_empty());

    let missing = fractal::sap::package::get_package_contents(&mut sap, "zsample_missing")
        .await
        .unwrap_err();
    assert_eq!(missing.code(), "package_not_found");
}
