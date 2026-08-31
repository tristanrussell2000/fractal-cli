//! Object creation as a shell: one POST to the collection, then a read-back.
//!
//! The exact request body is this suite's central assertion. Creation is the
//! one workflow whose payload has not yet been confirmed against a live
//! backend, so these tests document what Fractal currently sends rather than
//! what SAP is known to accept — if a live create disagrees, fix
//! `creation_payload` and these expectations together.

use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        editable_source::EditableAdtObjectType,
        object_creation::{AdtObjectCreationError, AdtObjectCreationRequest, create_adt_object},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string, header, method, path, query_param, query_param_is_missing},
};

const COLLECTION_PATH: &str = "/sap/bc/adt/programs/programs";
const OBJECT_PATH: &str = "/sap/bc/adt/programs/programs/zsample";
const PROGRAM_MEDIA_TYPE: &str = "application/vnd.sap.adt.programs.programs.v2+xml";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "903".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

fn creation_request(transport: Option<&str>) -> AdtObjectCreationRequest {
    AdtObjectCreationRequest {
        object_type: EditableAdtObjectType::Program,
        name: "zsample".to_owned(),
        package: "ZPKG".to_owned(),
        description: "Sample report".to_owned(),
        transport: transport.map(str::to_owned),
    }
}

fn expected_body() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><program:abapProgram xmlns:program=\"http://www.sap.com/adt/programs/programs\" xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:description=\"Sample report\" adtcore:name=\"ZSAMPLE\" adtcore:type=\"PROG/P\"><adtcore:packageRef adtcore:name=\"ZPKG\"/></program:abapProgram>"
        .to_owned()
}

async fn mount_csrf_session(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "create-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=create-test; Path=/"),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_read_back(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(OBJECT_PATH))
        .and(query_param("sap-client", "903"))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn creates_a_program_shell_and_verifies_it_exists() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(COLLECTION_PATH))
        .and(query_param("sap-client", "903"))
        .and(query_param_is_missing("corrNr"))
        .and(header("x-csrf-token", "create-csrf"))
        .and(header("cookie", "SAP_SESSIONID=create-test"))
        .and(header("content-type", PROGRAM_MEDIA_TYPE))
        .and(body_string(expected_body()))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    mount_read_back(
        &server,
        ResponseTemplate::new(200).set_body_string("<program:abapProgram/>"),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = create_adt_object(&mut client, &["Z*".to_owned()], &creation_request(None))
        .await
        .unwrap();

    assert_eq!(result.identity.name, "ZSAMPLE");
    assert_eq!(result.identity.object_uri, OBJECT_PATH);
    assert_eq!(result.package, "ZPKG");
    assert_eq!(result.description, "Sample report");
    assert_eq!(result.transport, None);
    server.verify().await;
}

#[tokio::test]
async fn sends_the_transport_as_corr_nr_on_the_create_request() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(COLLECTION_PATH))
        .and(query_param("corrNr", "DE3K900575"))
        .and(body_string(expected_body()))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    mount_read_back(
        &server,
        ResponseTemplate::new(200).set_body_string("<program:abapProgram/>"),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = create_adt_object(
        &mut client,
        &["Z*".to_owned()],
        &creation_request(Some("de3k900575")),
    )
    .await
    .unwrap();

    assert_eq!(result.transport.as_deref(), Some("DE3K900575"));
    server.verify().await;
}

#[tokio::test]
async fn creation_never_writes_source_or_activates() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(COLLECTION_PATH))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    mount_read_back(
        &server,
        ResponseTemplate::new(200).set_body_string("<program:abapProgram/>"),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    create_adt_object(&mut client, &["Z*".to_owned()], &creation_request(None))
        .await
        .unwrap();

    // The whole point of the shell shape: no PUT, no lock, no activation.
    let requests = server.received_requests().await.unwrap();
    for request in &requests {
        assert_ne!(
            request.method,
            wiremock::http::Method::PUT,
            "creation wrote source"
        );
        assert!(
            !request.url.path().contains("/activation"),
            "creation activated the object"
        );
        assert!(
            request.url.query_pairs().all(|(key, _)| key != "_action"),
            "creation locked the object"
        );
    }
    assert_eq!(
        requests.len(),
        3,
        "expected discovery, create, and read-back"
    );
}

#[tokio::test]
async fn a_rejected_creation_is_reported_without_a_read_back() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(COLLECTION_PATH))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<error><message>Not authorized to create objects in package ZPKG</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = create_adt_object(&mut client, &["Z*".to_owned()], &creation_request(None))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdtObjectCreationError::CreateRequest { .. }
    ));
    assert_eq!(error.code(), "edit_create_request_failed");
    assert_eq!(error.status(), Some(403));
    assert!(error.message().contains("Not authorized"));
    assert!(error.hint().unwrap().contains("fractal edit set"));
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal object search ZSAMPLE --kind PROG")
    );
    // Only discovery and the failed create; nothing was read back.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    server.verify().await;
}

#[tokio::test]
async fn a_creation_that_cannot_be_read_back_is_not_reported_as_success() {
    let server = MockServer::start().await;
    mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(COLLECTION_PATH))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    mount_read_back(
        &server,
        ResponseTemplate::new(404).set_body_string("<error><message>Not found</message></error>"),
    )
    .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = create_adt_object(&mut client, &["Z*".to_owned()], &creation_request(None))
        .await
        .unwrap_err();

    assert!(matches!(error, AdtObjectCreationError::Verification { .. }));
    assert_eq!(error.code(), "edit_create_verification_failed");
    assert!(error.hint().unwrap().contains("a second create would fail"));
    server.verify().await;
}

#[tokio::test]
async fn validates_everything_local_before_any_request() {
    let server = MockServer::start().await;

    let outside_namespace = AdtObjectCreationRequest {
        name: "SAP_STANDARD".to_owned(),
        ..creation_request(None)
    };
    let bad_package = AdtObjectCreationRequest {
        package: "ZPKG\"/><evil".to_owned(),
        ..creation_request(None)
    };
    let blank_description = AdtObjectCreationRequest {
        description: "   ".to_owned(),
        ..creation_request(None)
    };
    let bad_transport = AdtObjectCreationRequest {
        transport: Some("not a request".to_owned()),
        ..creation_request(None)
    };

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    for (request, expected) in [
        (outside_namespace, "object_outside_customer_namespaces"),
        (bad_package, "invalid_package_name"),
        (blank_description, "blank_object_description"),
        (bad_transport, "invalid_transport_request"),
    ] {
        let error = create_adt_object(&mut client, &["Z*".to_owned()], &request)
            .await
            .unwrap_err();
        assert_eq!(error.code(), expected);
        assert_eq!(error.status(), None);
    }

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "local validation must not contact SAP"
    );
}

#[tokio::test]
async fn refuses_object_types_whose_payload_is_not_implemented() {
    let server = MockServer::start().await;
    let request = AdtObjectCreationRequest {
        object_type: EditableAdtObjectType::Table,
        name: "ZTABLE".to_owned(),
        ..creation_request(None)
    };

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = create_adt_object(&mut client, &["Z*".to_owned()], &request)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "unsupported_create_object_type");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "an unsupported type must not reach SAP"
    );
}
