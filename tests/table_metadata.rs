use fractal::reportable_error::ReportableError;
use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        table::{TableError, TableMetadataOptions, get_table_metadata},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{basic_auth, body_string, body_string_contains, header, method, path, query_param},
};

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "903".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        password_command: None,
        edit_packages: None,
        allow_temporary_package: true,
        customer_namespaces: vec!["Z*".to_owned(), "Y*".to_owned()],
    }
}

async fn mount_discovery(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "metadata-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=metadata-test; Path=/"),
        )
        .expect(1)
        .mount(server)
        .await;
}

/// The recorded field list, which is what decides the fields, their order and
/// which are key. Read from DD03L so includes and appends are already
/// flattened.
async fn mount_fields(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string_contains("FROM DD03L"))
        .and(body_string_contains("TABNAME = 'ZSAMPLE_RECORD'"))
        .and(body_string_contains("AS4LOCAL = 'A'"))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "metadata-csrf"))
        .and(header("cookie", "SAP_SESSIONID=metadata-test"))
        .and(basic_auth("developer", "password"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:columns><dataPreview:metadata dataPreview:name="POSITION"/><dataPreview:dataSet><dataPreview:data>0002</dataPreview:data><dataPreview:data>0001</dataPreview:data><dataPreview:data>0003</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="FIELDNAME"/><dataPreview:dataSet><dataPreview:data>STATUS</dataPreview:data><dataPreview:data>CLIENT</dataPreview:data><dataPreview:data>.INCLU--AP</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="KEYFLAG"/><dataPreview:dataSet><dataPreview:data/><dataPreview:data>X</dataPreview:data><dataPreview:data/></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="ROLLNAME"/><dataPreview:dataSet><dataPreview:data>ZSAMPLE_STATUS</dataPreview:data><dataPreview:data/><dataPreview:data/></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="DATATYPE"/><dataPreview:dataSet><dataPreview:data>CHAR</dataPreview:data><dataPreview:data>CLNT</dataPreview:data><dataPreview:data/></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="LENG"/><dataPreview:dataSet><dataPreview:data>000012</dataPreview:data><dataPreview:data>000003</dataPreview:data><dataPreview:data>000000</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_ddic_preview(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/ddic"))
        .and(query_param("rowNumber", "1"))
        .and(query_param("ddicEntityName", "ZSAMPLE_RECORD"))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "metadata-csrf"))
        .and(header("cookie", "SAP_SESSIONID=metadata-test"))
        .and(basic_auth("developer", "password"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:name>ZSAMPLE_RECORD</dataPreview:name><dataPreview:columns><dataPreview:metadata dataPreview:name="STATUS" dataPreview:type="C" dataPreview:description="Status" dataPreview:colType="CHAR" dataPreview:length="12"/></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="CLIENT" dataPreview:type="C" dataPreview:description="Client" dataPreview:colType="CLNT" dataPreview:length="3"/></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn combines_the_recorded_field_list_with_ddic_preview_metadata() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;
    mount_fields(&server).await;
    mount_ddic_preview(&server).await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let metadata = get_table_metadata(
        &mut client,
        " zsample_record ",
        &TableMetadataOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(metadata.entity, "zsample_record");
    assert_eq!(metadata.total_rows, None);
    assert_eq!(metadata.fields.len(), 2);
    assert_eq!(metadata.fields[0].name, "client");
    assert!(metadata.fields[0].is_key);
    assert_eq!(metadata.fields[0].declared_type, "abap.clnt");
    assert_eq!(metadata.fields[0].col_type.as_deref(), Some("CLNT"));
    assert_eq!(metadata.fields[0].length, Some(3));
    assert_eq!(metadata.fields[1].name, "status");
    assert_eq!(metadata.fields[1].declared_type, "zsample_status");
    assert_eq!(metadata.fields[1].description.as_deref(), Some("Status"));
    server.verify().await;
}

#[tokio::test]
async fn includes_an_accurate_row_count_only_when_requested() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;
    mount_fields(&server).await;
    mount_ddic_preview(&server).await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(query_param("rowNumber", "1"))
        .and(query_param("dataAging", "true"))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "metadata-csrf"))
        .and(header("cookie", "SAP_SESSIONID=metadata-test"))
        .and(body_string(
            "SELECT COUNT(*) AS ROW_COUNT\nFROM ZSAMPLE_RECORD",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:columns><dataPreview:metadata dataPreview:name="ROW_COUNT"/><dataPreview:dataSet><dataPreview:data>42</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let metadata = get_table_metadata(
        &mut client,
        "ZSAMPLE_RECORD",
        &TableMetadataOptions {
            include_row_count: true,
        },
    )
    .await
    .unwrap();

    assert_eq!(metadata.total_rows, Some(42));
    server.verify().await;
}

#[tokio::test]
async fn reports_a_requested_count_without_a_numeric_value() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;
    mount_fields(&server).await;
    mount_ddic_preview(&server).await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:columns><dataPreview:metadata dataPreview:name="ROW_COUNT"/><dataPreview:dataSet><dataPreview:data>not-a-number</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = get_table_metadata(
        &mut client,
        "ZSAMPLE_RECORD",
        &TableMetadataOptions {
            include_row_count: true,
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(error, TableError::CountMissing));
    assert_eq!(error.code(), "table_count_response_error");
    server.verify().await;
}

#[tokio::test]
async fn reports_malformed_ddic_preview_metadata() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;
    mount_fields(&server).await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/ddic"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<not-closed"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = get_table_metadata(
        &mut client,
        "ZSAMPLE_RECORD",
        &TableMetadataOptions::default(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, TableError::Parse(_)));
    server.verify().await;
}
