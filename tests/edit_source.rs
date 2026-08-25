use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        edit::{
            AdtSourceReadError, AdtSourceVersion, EditableAdtObjectType, read_adt_source_for_edit,
        },
    },
    source_change::source_sha256,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "903".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned(), "/ACME/*".to_owned()],
    }
}

#[tokio::test]
async fn fetches_complete_active_class_source_with_exact_hash_metadata() {
    let server = MockServer::start().await;
    let source = "CLASS zcl_example IMPLEMENTATION.\r\n  \" café\r\nENDCLASS.\r\n";
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_example/source/main"))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(source.as_bytes()))
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = read_adt_source_for_edit(
        &client,
        EditableAdtObjectType::Class,
        " zcl_example ",
        AdtSourceVersion::Active,
    )
    .await
    .unwrap();

    assert_eq!(result.object_type, EditableAdtObjectType::Class);
    assert_eq!(result.name, "ZCL_EXAMPLE");
    assert_eq!(result.object_uri, "/sap/bc/adt/oo/classes/zcl_example");
    assert_eq!(
        result.source_uri,
        "/sap/bc/adt/oo/classes/zcl_example/source/main"
    );
    assert_eq!(result.requested_version, AdtSourceVersion::Active);
    assert_eq!(result.source, source);
    assert_eq!(result.bytes, source.len());
    assert_eq!(result.sha256, source_sha256(source));
    server.verify().await;
}

#[tokio::test]
async fn requests_inactive_namespaced_ddl_source_with_an_encoded_name() {
    let server = MockServer::start().await;
    let source = "define view entity /ACME/EXAMPLE as select from ztable { key id }";
    Mock::given(method("GET"))
        .and(path(
            "/sap/bc/adt/ddic/ddl/sources/%2facme%2fexample/source/main",
        ))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_string(source))
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = read_adt_source_for_edit(
        &client,
        EditableAdtObjectType::DdlSource,
        "/ACME/EXAMPLE",
        AdtSourceVersion::Inactive,
    )
    .await
    .unwrap();

    assert_eq!(result.name, "/ACME/EXAMPLE");
    assert_eq!(result.requested_version, AdtSourceVersion::Inactive);
    assert_eq!(result.source, source);
    server.verify().await;
}

#[tokio::test]
async fn rejects_an_invalid_name_before_an_http_request() {
    let client = SapClient::new(
        &profile("http://127.0.0.1:1".to_owned()),
        "password".to_owned(),
    )
    .unwrap();

    let error = read_adt_source_for_edit(
        &client,
        EditableAdtObjectType::Program,
        "ZREPORT;DELETE",
        AdtSourceVersion::Active,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, AdtSourceReadError::InvalidTarget(_)));
    assert_eq!(error.code(), "invalid_edit_object_name");
}

#[tokio::test]
async fn rejects_non_utf8_source_without_hashing_decoded_replacement_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zbad/source/main"))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes([0xFF, 0xFE, b'A']))
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = read_adt_source_for_edit(
        &client,
        EditableAdtObjectType::Program,
        "ZBAD",
        AdtSourceVersion::Inactive,
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        AdtSourceReadError::InvalidSourceEncoding { .. }
    ));
    assert_eq!(error.code(), "edit_source_encoding_error");
    server.verify().await;
}

#[tokio::test]
async fn preserves_sap_errors_from_the_source_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/interfaces/zif_missing/source/main"))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Interface does not exist"))
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = read_adt_source_for_edit(
        &client,
        EditableAdtObjectType::Interface,
        "ZIF_MISSING",
        AdtSourceVersion::Active,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, AdtSourceReadError::Sap(_)));
    assert_eq!(error.code(), "not_found");
    assert!(error.sap_error().is_some());
    server.verify().await;
}
