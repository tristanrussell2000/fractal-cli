use fractal::{
    config::Profile,
    sap::{
        adt::{ObjectSearchOptions, RepositoryKind, search_objects},
        client::SapClient,
    },
};
use std::time::{Duration, Instant};

use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{basic_auth, method, path, query_param},
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

const SEARCH_PATH: &str = "/sap/bc/adt/repository/informationsystem/search";

#[tokio::test]
async fn search_objects_queries_adt_and_filters_deduplicates_and_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SEARCH_PATH))
        .and(query_param("operation", "quickSearch"))
        .and(query_param("maxResults", "500"))
        .and(query_param("query", "VERSION*"))
        .and(basic_auth("developer", "password"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<r:objectReferences xmlns:r="urn:test">
                <r:objectReference name="ZCL_VERSION" type="CLAS/OC" description="Class" packageName="ZAPP" uri="/sap/bc/adt/oo/classes/zcl_version"/>
                <r:objectReference name="SAP_VERSION" type="CLAS/OC" packageName="SAPP" uri="/sap/bc/adt/oo/classes/sap_version"/>
                <r:objectReference name="ZTABLE_VERSION" type="TABL/DT" packageName="ZAPP" uri="/sap/bc/adt/ddic/tables/ztable_version"/>
            </r:objectReferences>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(SEARCH_PATH))
        .and(query_param("operation", "quickSearch"))
        .and(query_param("maxResults", "500"))
        .and(query_param("query", "Z*VERSION*"))
        .and(basic_auth("developer", "password"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<r:objectReferences xmlns:r="urn:test">
                <r:objectReference name="ZCL_VERSION" type="CLAS/OC" description="Class" packageName="ZAPP" uri="/sap/bc/adt/oo/classes/zcl_version"/>
                <r:objectReference name="ZCLASS_VERSION" type="CLAS/OC" packageName="ZAPP" uri="/sap/bc/adt/oo/classes/zclass_version"/>
            </r:objectReferences>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = search_objects(
        &mut client,
        &profile,
        "VERSION",
        ObjectSearchOptions {
            kind: Some(RepositoryKind::Clas),
            offset: 1,
            limit: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.total, 2);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].name, "ZCL_VERSION");
    assert_eq!(result.hits[0].kind, RepositoryKind::Clas);
    assert!(!result.possibly_truncated_by_sap_cap);
    server.verify().await;
}

#[tokio::test]
async fn search_requests_run_concurrently() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SEARCH_PATH))
        .and(query_param("operation", "quickSearch"))
        .and(query_param("maxResults", "500"))
        .and(basic_auth("developer", "password"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"<r:objectReferences xmlns:r="urn:test"><r:objectReference name="ZOBJECT" type="CLAS/OC" packageName="ZAPP" uri="/sap/bc/adt/oo/classes/zobject"/></r:objectReferences>"#,
                )
                .set_delay(Duration::from_millis(150)),
        )
        .expect(3)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let started = Instant::now();
    search_objects(
        &mut client,
        &profile,
        "OBJECT",
        ObjectSearchOptions {
            limit: Some(10),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(started.elapsed() < Duration::from_millis(400));
    server.verify().await;
}

#[tokio::test]
async fn search_objects_warns_when_a_sap_response_reaches_the_cap() {
    let server = MockServer::start().await;
    let references = (0..500)
        .map(|index| {
            format!(
                r#"<r:objectReference name="ZOBJECT_{index}" type="CLAS/OC" packageName="ZAPP" uri="/sap/bc/adt/oo/classes/zobject_{index}"/>"#
            )
        })
        .collect::<String>();
    let body =
        format!(r#"<r:objectReferences xmlns:r="urn:test">{references}</r:objectReferences>"#);

    Mock::given(method("GET"))
        .and(path(SEARCH_PATH))
        .and(query_param("operation", "quickSearch"))
        .and(query_param("maxResults", "500"))
        .and(basic_auth("developer", "password"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(3)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = search_objects(
        &mut client,
        &profile,
        "OBJECT",
        ObjectSearchOptions {
            limit: Some(10),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(result.possibly_truncated_by_sap_cap);
    assert_eq!(result.sap_search_cap, 500);
    assert_eq!(result.total, 500);
    assert_eq!(result.hits.len(), 10);
    server.verify().await;
}
