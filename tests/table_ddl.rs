use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        table::{TableDdlParseError, TableError, get_table_ddl},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "903".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned(), "Y*".to_owned()],
    }
}

#[tokio::test]
async fn fetches_and_parses_a_table_ddl_source() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/tables/zsample_record/source/main"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"
@EndUserText.label: 'Synthetic record'
define table zsample_record {
  key client : abap.clnt not null;
  status     : zsample_status;
}
"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let ddl = get_table_ddl(&client, " zsample_record ").await.unwrap();

    assert_eq!(ddl.name, "zsample_record");
    assert_eq!(ddl.fields.len(), 2);
    assert_eq!(ddl.fields[0].name, "client");
    assert!(ddl.fields[0].is_key);
    assert_eq!(ddl.fields[1].declared_type, "zsample_status");
    server.verify().await;
}

#[tokio::test]
async fn encodes_namespaced_table_names_as_one_adt_path_component() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/sap/bc/adt/ddic/tables/%2fsample%2frecord/source/main",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("define table /sample/record { key id : abap.int4; }"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let ddl = get_table_ddl(&client, "/SAMPLE/RECORD").await.unwrap();

    assert_eq!(ddl.name, "/sample/record");
    server.verify().await;
}

#[tokio::test]
async fn rejects_invalid_table_names_before_requesting_source() {
    let client = SapClient::new(
        &profile("http://127.0.0.1:1".to_owned()),
        "password".to_owned(),
    )
    .unwrap();

    let error = get_table_ddl(&client, "zsample_record;delete")
        .await
        .unwrap_err();

    assert!(matches!(error, TableError::InvalidEntityName(_)));
}

#[tokio::test]
async fn identifies_malformed_table_source_as_a_ddl_parse_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/tables/zsample_broken/source/main"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("define table zsample_broken { key id : abap.int4;"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = get_table_ddl(&client, "ZSAMPLE_BROKEN").await.unwrap_err();

    assert!(matches!(
        &error,
        TableError::DdlParse(TableDdlParseError::UnterminatedBody { .. })
    ));
    assert_eq!(error.code(), "table_ddl_parse_error");
    server.verify().await;
}

#[tokio::test]
async fn preserves_sap_errors_from_the_source_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/tables/zsample_missing/source/main"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Table does not exist"))
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = get_table_ddl(&client, "ZSAMPLE_MISSING").await.unwrap_err();

    assert!(matches!(&error, TableError::DdlSource(_)));
    assert_eq!(error.code(), "not_found");
    assert!(error.sap_error().is_some());
    server.verify().await;
}
