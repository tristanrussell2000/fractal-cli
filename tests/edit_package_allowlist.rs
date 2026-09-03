//! The per-profile package allowlist.
//!
//! The assertions that matter are the refusals, and specifically *what did not
//! happen*: a refused edit must issue no lock and no write. Every mock here is
//! mounted with an exact expected count, so an extra request fails the test as
//! loudly as a missing one — which is how "the guard costs nothing when it is
//! not configured" is checked at all.

use fractal::config::{EditPolicy, Profile};
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    client::SapClient,
    editable_source::EditableAdtObjectType,
    object_creation::{AdtObjectCreationRequest, create_adt_object},
    object_deletion::{AdtObjectDeletionRequest, delete_adt_object},
    source_patch::{AdtSourcePatchRequest, patch_adt_source_atomically},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

const OBJECT_PATH: &str = "/sap/bc/adt/programs/programs/zsample";
const SOURCE_PATH: &str = "/sap/bc/adt/programs/programs/zsample/source/main";
const USAGES_PATH: &str = "/sap/bc/adt/repository/informationsystem/usageReferences";
const CREATE_PATH: &str = "/sap/bc/adt/programs/programs";

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

fn restricted(packages: &[&str], allow_temporary_package: bool) -> EditPolicy {
    EditPolicy {
        customer_namespaces: vec!["Z*".to_owned()],
        edit_packages: Some(packages.iter().map(|p| (*p).to_owned()).collect()),
        allow_temporary_package,
    }
}

fn creation_request(package: &str) -> AdtObjectCreationRequest {
    AdtObjectCreationRequest {
        object_type: EditableAdtObjectType::Program,
        name: "zsample".to_owned(),
        package: package.to_owned(),
        description: "Sample report".to_owned(),
        transport: None,
    }
}

fn deletion_request() -> AdtObjectDeletionRequest {
    AdtObjectDeletionRequest {
        object_type: EditableAdtObjectType::Program,
        name: "zsample".to_owned(),
        transport: None,
        force: false,
    }
}

fn patch_request() -> AdtSourcePatchRequest {
    AdtSourcePatchRequest {
        object_type: EditableAdtObjectType::Program,
        name: "zsample".to_owned(),
        find: "old".to_owned(),
        replace: "new".to_owned(),
        expected_sha256: None,
        transport: None,
    }
}

fn object_xml(package: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<program:abapProgram xmlns:program="http://www.sap.com/adt/programs/programs" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE" adtcore:type="PROG/P">
  <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/x" adtcore:type="DEVC/K" adtcore:name="{package}"/>
</program:abapProgram>"#
    )
}

async fn mount_csrf_session(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "allowlist-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=allowlist-test; Path=/"),
        )
        .mount(server)
        .await;
}

/// Mounts the object read the guard uses to find out where an object lives.
async fn mount_package_read(server: &MockServer, package: &str, times: u64) {
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(object_xml(package)))
        .expect(times)
        .mount(server)
        .await;
}

async fn client(server: &MockServer) -> SapClient {
    SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap()
}

#[tokio::test]
async fn creating_outside_the_granted_packages_issues_no_request_at_all() {
    let server = MockServer::start().await;
    // Nothing is mounted: creation knows its package from the argument, so a
    // refusal must not even reach the network.

    let error = create_adt_object(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], true),
        &creation_request("ZOTHER"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_edit_packages");
    let hint = error.hint().unwrap();
    assert!(hint.contains("ZOTHER"), "{hint}");
    assert!(hint.contains("ZPROJ*"), "{hint}");
    server.verify().await;
}

#[tokio::test]
async fn creating_inside_a_granted_package_proceeds() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(CREATE_PATH))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    mount_package_read(&server, "ZPROJ_CORE", 1).await;

    create_adt_object(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], true),
        &creation_request("ZPROJ_CORE"),
    )
    .await
    .unwrap();

    server.verify().await;
}

#[tokio::test]
async fn scratch_objects_stay_creatable_under_a_restrictive_list() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(CREATE_PATH))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    mount_package_read(&server, "$TMP", 1).await;

    // $TMP is local throwaway work, not shared code. An allowlist that blocked
    // it would mostly be in the way.
    create_adt_object(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], true),
        &creation_request("$TMP"),
    )
    .await
    .unwrap();

    server.verify().await;
}

#[tokio::test]
async fn scratch_access_can_be_turned_off_per_profile() {
    let server = MockServer::start().await;

    let error = create_adt_object(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], false),
        &creation_request("$TMP"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_edit_packages");
    server.verify().await;
}

#[tokio::test]
async fn granting_no_packages_refuses_everything_but_scratch() {
    let server = MockServer::start().await;

    let error = create_adt_object(
        &mut client(&server).await,
        &restricted(&[], true),
        &creation_request("ZPROJ_CORE"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_edit_packages");
    assert!(error.hint().unwrap().contains("no packages at all"));
    server.verify().await;
}

#[tokio::test]
async fn a_refused_delete_never_reaches_the_where_used_check_or_the_lock() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_package_read(&server, "ZOTHER", 1).await;
    // Neither of these may be requested. The package check runs before the
    // where-used lookup so the cheap refusal comes first, and before the lock
    // so a refusal never leaves one behind.
    Mock::given(method("POST"))
        .and(path(USAGES_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let error = delete_adt_object(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], true),
        &deletion_request(),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_edit_packages");
    server.verify().await;
}

#[tokio::test]
async fn a_refused_patch_takes_no_lock() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    mount_package_read(&server, "ZOTHER", 1).await;
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(SOURCE_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let error = patch_adt_source_atomically(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], true),
        &patch_request(),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_edit_packages");
    server.verify().await;
}

#[tokio::test]
async fn an_unrestricted_profile_never_looks_a_package_up() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(USAGES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<usageReferences:usageReferenceResult xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences"><usageReferences:referencedObjects/></usageReferences:usageReferenceResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<lockResult><LOCK_HANDLE>2026090312000000</LOCK_HANDLE></lockResult>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // The only GET of the object is the post-delete proof that it is gone. If
    // the guard looked the package up for a profile that grants everything,
    // this 404 would come back first and the delete would fail instead.
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(404).set_body_string("<error>Not found</error>"))
        .expect(1)
        .mount(&server)
        .await;

    delete_adt_object(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &deletion_request(),
    )
    .await
    .unwrap();

    server.verify().await;
}

#[tokio::test]
async fn an_unrestricted_profile_pays_for_no_extra_request() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(CREATE_PATH))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    // Exactly one GET: the creation read-back. If the guard looked the package
    // up for a profile that grants everything, this would see two.
    mount_package_read(&server, "ZOTHER", 1).await;

    create_adt_object(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &creation_request("ZOTHER"),
    )
    .await
    .unwrap();

    server.verify().await;
}

#[tokio::test]
async fn an_object_that_reports_no_package_is_refused_rather_than_allowed() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<program:abapProgram xmlns:program="urn:p" xmlns:adtcore="urn:a" adtcore:name="ZSAMPLE"/>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    // Fail closed: a package that cannot be proven is not an authorized one.
    let error = delete_adt_object(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], true),
        &deletion_request(),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_package_unknown");
    server.verify().await;
}

#[tokio::test]
async fn a_failed_package_lookup_refuses_the_edit() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal error"))
        .expect(1)
        .mount(&server)
        .await;

    let error = delete_adt_object(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], true),
        &deletion_request(),
    )
    .await
    .unwrap_err();

    // Not "not allowed" — the check could not be made, which is a different
    // thing to tell the caller, but it stops the edit just the same.
    assert_eq!(error.code(), "object_package_lookup_failed");
    assert_eq!(error.status(), Some(500));
    server.verify().await;
}

#[tokio::test]
async fn the_namespace_floor_still_applies_inside_a_granted_package() {
    let server = MockServer::start().await;

    let outside_namespace = AdtObjectCreationRequest {
        name: "sapsample".to_owned(),
        ..creation_request("ZPROJ_CORE")
    };
    let error = create_adt_object(
        &mut client(&server).await,
        &restricted(&["ZPROJ*"], true),
        &outside_namespace,
    )
    .await
    .unwrap_err();

    // Granting a package does not lift the name check: the two guards are
    // ANDed, a floor and a grant rather than one replacing the other.
    assert_eq!(error.code(), "object_outside_customer_namespaces");
    server.verify().await;
}
