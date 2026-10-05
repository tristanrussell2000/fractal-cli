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

fn column_xml(name: &str, values: &[&str]) -> String {
    let data: String = values
        .iter()
        .map(|value| format!("<dataPreview:data>{value}</dataPreview:data>"))
        .collect();
    format!(
        r#"<dataPreview:columns><dataPreview:metadata dataPreview:name="{name}"/><dataPreview:dataSet>{data}</dataPreview:dataSet></dataPreview:columns>"#
    )
}

/// A DD03L result: one `.INCLUDE` marker and the two fields it contributed.
/// `CLIENT` carries no data element, so its declared type has to be built from
/// the DDIC type.
fn field_rows() -> String {
    let columns = [
        ("POSITION", vec!["0001", "0002", "0003"]),
        ("FIELDNAME", vec![".INCLUDE", "CLIENT", "STATUS"]),
        ("KEYFLAG", vec!["", "X", ""]),
        ("ROLLNAME", vec!["", "", "ZSAMPLE_STATUS"]),
        ("DOMNAME", vec!["", "", "ZSAMPLE_DOM"]),
        ("DATATYPE", vec!["", "CLNT", "CHAR"]),
        ("LENG", vec!["000000", "000003", "000012"]),
        ("DECIMALS", vec!["000000", "000000", "000000"]),
        ("INTTYPE", vec!["", "C", "C"]),
        ("NOTNULL", vec!["", "X", ""]),
        ("CHECKTABLE", vec!["", "*", ""]),
        ("DDTEXT", vec!["", "Client", "Status"]),
        ("TABCLASS", vec!["TRANSP", "TRANSP", "TRANSP"]),
    ];

    let mut xml = String::from(
        r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview">"#,
    );
    for (name, values) in columns {
        xml.push_str(&column_xml(name, &values));
    }
    xml.push_str("</dataPreview:tableData>");
    xml
}

/// The recorded field list, which is the only read `table metadata` makes for
/// fields — there is no data preview on this path.
async fn mount_fields(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string_contains("dd03l"))
        .and(body_string_contains("tabname = 'ZSAMPLE_RECORD'"))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "metadata-csrf"))
        .and(header("cookie", "SAP_SESSIONID=metadata-test"))
        .and(basic_auth("developer", "password"))
        .respond_with(ResponseTemplate::new(200).set_body_string(field_rows()))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn reads_the_recorded_field_list_with_no_data_preview() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;
    mount_fields(&server).await;

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
    // Three rows, two fields: the `.INCLUDE` marker is not one.
    assert_eq!(metadata.fields.len(), 2);
    assert_eq!(metadata.fields[0].name, "client");
    assert!(metadata.fields[0].is_key);
    // No data element, so the declared type is built from the DDIC type — the
    // spelling the client-column check recognises.
    assert_eq!(metadata.fields[0].declared_type, "abap.clnt");
    assert_eq!(metadata.fields[0].col_type.as_deref(), Some("CLNT"));
    assert_eq!(metadata.fields[0].sap_type.as_deref(), Some("C"));
    assert_eq!(metadata.fields[0].length, Some(3));
    assert_eq!(metadata.fields[0].description.as_deref(), Some("Client"));
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

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string(
            "SELECT COUNT(*) AS ROW_COUNT\nFROM ZSAMPLE_RECORD",
        ))
        .and(query_param("rowNumber", "1"))
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

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string(
            "SELECT COUNT(*) AS ROW_COUNT\nFROM ZSAMPLE_RECORD",
        ))
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
async fn reports_a_malformed_field_list_response() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
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

/// A field list that came back with columns but no rows is a name that does
/// not exist, not a table with no fields.
#[tokio::test]
async fn reports_a_name_that_records_no_fields() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;

    let empty: String = [
        "POSITION",
        "FIELDNAME",
        "KEYFLAG",
        "ROLLNAME",
        "DOMNAME",
        "DATATYPE",
        "LENG",
        "DECIMALS",
        "INTTYPE",
        "NOTNULL",
        "CHECKTABLE",
        "DDTEXT",
        "TABCLASS",
    ]
    .iter()
    .map(|name| column_xml(name, &[]))
    .collect();
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview">{empty}</dataPreview:tableData>"#
        )))
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

    assert!(matches!(error, TableError::EntityFieldsMissing { .. }));
    assert_eq!(error.code(), "table_fields_missing");
    server.verify().await;
}
