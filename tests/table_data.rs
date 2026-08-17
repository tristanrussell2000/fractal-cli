use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        table::{TableDataOptions, TableDataQuery, get_table_data},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{basic_auth, body_string, header, method, path, query_param},
};

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "100".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned(), "Y*".to_owned()],
    }
}

async fn mount_discovery(server: &MockServer, token: &'static str, cookie: &'static str) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", token)
                .insert_header("set-cookie", format!("SAP_SESSIONID={cookie}; Path=/")),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn unfiltered_simple_mode_uses_ddic_preview_and_pages_locally() {
    let server = MockServer::start().await;
    mount_discovery(&server, "table-csrf", "table-test").await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/ddic"))
        .and(query_param("rowNumber", "3"))
        .and(query_param("ddicEntityName", "ZDEMO_EVENT_LOG"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "table-csrf"))
        .and(header("cookie", "SAP_SESSIONID=table-test"))
        .and(basic_auth("developer", "password"))
        .and(body_string(""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>0</dataPreview:totalRows><dataPreview:name>ZDEMO_EVENT_LOG</dataPreview:name><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID" dataPreview:type="N" dataPreview:colType="NUMC" dataPreview:length="10"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data><dataPreview:data>0000000002</dataPreview:data><dataPreview:data>0000000003</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(query_param("rowNumber", "1"))
        .and(query_param("dataAging", "true"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "table-csrf"))
        .and(header("cookie", "SAP_SESSIONID=table-test"))
        .and(body_string(
            "SELECT COUNT(*) AS ROW_COUNT\nFROM ZDEMO_EVENT_LOG",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:columns><dataPreview:metadata dataPreview:name="ROW_COUNT"/><dataPreview:dataSet><dataPreview:data>42</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_table_data(
        &mut client,
        " zdemo_event_log ",
        &TableDataOptions {
            query: TableDataQuery::default(),
            offset: 1,
            limit: 2,
        },
    )
    .await
    .unwrap();

    assert_eq!(result.entity.as_deref(), Some("ZDEMO_EVENT_LOG"));
    assert_eq!(result.total_rows, Some(42));
    assert_eq!(
        result.rows,
        vec![vec!["0000000002".to_owned()], vec!["0000000003".to_owned()]]
    );
    server.verify().await;
}

#[tokio::test]
async fn filtered_simple_mode_builds_and_posts_a_freestyle_query() {
    let server = MockServer::start().await;
    mount_discovery(&server, "filter-csrf", "filter-test").await;

    let expected_query = "SELECT EVENT_ID, STATUS\nFROM ZDEMO_EVENT_LOG\nWHERE STATUS = 'OPEN'";
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(query_param("rowNumber", "2"))
        .and(query_param("dataAging", "true"))
        .and(query_param("sap-client", "100"))
        .and(header("content-type", "text/plain; charset=utf-8"))
        .and(header("x-csrf-token", "filter-csrf"))
        .and(header("cookie", "SAP_SESSIONID=filter-test"))
        .and(basic_auth("developer", "password"))
        .and(body_string(expected_query))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>2</dataPreview:totalRows><dataPreview:executedQueryString>SELECT EVENT_ID, STATUS FROM ZDEMO_EVENT_LOG WHERE STATUS = 'OPEN'</dataPreview:executedQueryString><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data><dataPreview:data>0000000002</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="STATUS"/><dataPreview:dataSet><dataPreview:data>OPEN</dataPreview:data><dataPreview:data>OPEN</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/ddic"))
        .and(query_param("rowNumber", "1"))
        .and(query_param("ddicEntityName", "ZDEMO_EVENT_LOG"))
        .and(query_param("sap-client", "100"))
        .and(header("x-csrf-token", "filter-csrf"))
        .and(header("cookie", "SAP_SESSIONID=filter-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID" dataPreview:type="N" dataPreview:colType="NUMC" dataPreview:length="10" dataPreview:description="Event ID"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="STATUS" dataPreview:type="C" dataPreview:colType="CHAR" dataPreview:length="10" dataPreview:description="Status"/><dataPreview:dataSet><dataPreview:data>OPEN</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_table_data(
        &mut client,
        "ZDEMO_EVENT_LOG",
        &TableDataOptions {
            query: TableDataQuery::Simple {
                fields: vec!["event_id".to_owned(), "status".to_owned()],
                where_clause: Some("STATUS='OPEN'".to_owned()),
            },
            offset: 0,
            limit: 2,
        },
    )
    .await
    .unwrap();

    assert_eq!(result.total_rows, Some(2));
    assert_eq!(result.columns[0].name, "EVENT_ID");
    assert_eq!(result.columns[0].sap_type.as_deref(), Some("N"));
    assert_eq!(result.columns[0].col_type.as_deref(), Some("NUMC"));
    assert_eq!(result.columns[0].length, Some(10));
    assert_eq!(result.columns[0].description.as_deref(), Some("Event ID"));
    assert_eq!(result.rows[0], vec!["0000000001", "OPEN"]);
    server.verify().await;
}

#[tokio::test]
async fn unfiltered_simple_mode_keeps_preview_when_count_enrichment_fails() {
    let server = MockServer::start().await;
    mount_discovery(&server, "count-failure-csrf", "count-failure-test").await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/ddic"))
        .and(query_param("rowNumber", "1"))
        .and(query_param("ddicEntityName", "ZDEMO_EVENT_LOG"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>1</dataPreview:totalRows><dataPreview:name>ZDEMO_EVENT_LOG</dataPreview:name><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string(
            "SELECT COUNT(*) AS ROW_COUNT\nFROM ZDEMO_EVENT_LOG",
        ))
        .respond_with(ResponseTemplate::new(500).set_body_string("count unavailable"))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_table_data(
        &mut client,
        "ZDEMO_EVENT_LOG",
        &TableDataOptions {
            query: TableDataQuery::default(),
            offset: 0,
            limit: 1,
        },
    )
    .await
    .unwrap();

    assert_eq!(result.total_rows, Some(1));
    assert_eq!(result.rows, vec![vec!["0000000001".to_owned()]]);
    server.verify().await;
}

#[tokio::test]
async fn filtered_simple_mode_keeps_query_result_when_metadata_enrichment_fails() {
    let server = MockServer::start().await;
    mount_discovery(&server, "metadata-failure-csrf", "metadata-failure-test").await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string(
            "SELECT EVENT_ID\nFROM ZDEMO_EVENT_LOG\nWHERE STATUS = 'OPEN'",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>1</dataPreview:totalRows><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/ddic"))
        .and(query_param("rowNumber", "1"))
        .and(query_param("ddicEntityName", "ZDEMO_EVENT_LOG"))
        .respond_with(ResponseTemplate::new(500).set_body_string("metadata unavailable"))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_table_data(
        &mut client,
        "ZDEMO_EVENT_LOG",
        &TableDataOptions {
            query: TableDataQuery::Simple {
                fields: vec!["EVENT_ID".to_owned()],
                where_clause: Some("STATUS='OPEN'".to_owned()),
            },
            offset: 0,
            limit: 1,
        },
    )
    .await
    .unwrap();

    assert_eq!(result.columns[0].sap_type, None);
    assert_eq!(result.columns[0].length, None);
    assert_eq!(result.rows, vec![vec!["0000000001".to_owned()]]);
    server.verify().await;
}

#[tokio::test]
async fn full_query_mode_preserves_literals_while_breaking_clause_lines() {
    let server = MockServer::start().await;
    mount_discovery(&server, "query-csrf", "query-test").await;

    let query =
        "SELECT EVENT_ID FROM ZDEMO_EVENT_LOG WHERE NOTE = 'FROM WHERE' ORDER   BY EVENT_ID";
    let expected_body =
        "SELECT EVENT_ID\nFROM ZDEMO_EVENT_LOG\nWHERE NOTE = 'FROM WHERE'\nORDER   BY EVENT_ID";
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(query_param("rowNumber", "1"))
        .and(query_param("dataAging", "true"))
        .and(query_param("sap-client", "100"))
        .and(header("content-type", "text/plain; charset=utf-8"))
        .and(header("x-csrf-token", "query-csrf"))
        .and(header("cookie", "SAP_SESSIONID=query-test"))
        .and(body_string(expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>1</dataPreview:totalRows><dataPreview:executedQueryString>SELECT EVENT_ID FROM ZDEMO_EVENT_LOG WHERE NOTE = 'FROM WHERE' ORDER BY EVENT_ID</dataPreview:executedQueryString><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_table_data(
        &mut client,
        "ZDEMO_EVENT_LOG",
        &TableDataOptions {
            query: TableDataQuery::Full(query.to_owned()),
            offset: 0,
            limit: 1,
        },
    )
    .await
    .unwrap();

    assert_eq!(result.rows, vec![vec!["0000000001".to_owned()]]);
    assert_eq!(
        result.executed_query.as_deref(),
        Some("SELECT EVENT_ID FROM ZDEMO_EVENT_LOG WHERE NOTE = 'FROM WHERE' ORDER BY EVENT_ID")
    );
    server.verify().await;
}
