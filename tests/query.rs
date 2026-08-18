use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        table::{QueryOptions, TableError, TableQueryErrorKind, run_query},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string, header, method, path, query_param},
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

async fn mount_discovery(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "query-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=query-test; Path=/"),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn standalone_query_preserves_literals_while_breaking_clause_lines() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;

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
    let result = run_query(
        &mut client,
        query,
        &QueryOptions {
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

#[tokio::test]
async fn standalone_query_structures_unknown_sources_without_an_entity() {
    let server = MockServer::start().await;
    mount_discovery(&server).await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string("SELECT *\nFROM ZUNKNOWN_DATA"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("<error><message>Cannot find 'ZUNKNOWN_DATA'</message></error>"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = run_query(
        &mut client,
        "SELECT * FROM ZUNKNOWN_DATA",
        &QueryOptions {
            offset: 0,
            limit: 1,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "table_query_unknown_source");
    assert!(matches!(
        error,
        TableError::Query { query, .. }
            if query.kind == TableQueryErrorKind::UnknownSource
                && query.identifier.as_deref() == Some("ZUNKNOWN_DATA")
    ));
    server.verify().await;
}
