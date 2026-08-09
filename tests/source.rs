use fractal::{
    config::Profile,
    sap::{
        adt::{AdtError, ByteRangeOptions, get_source},
        client::SapClient,
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
async fn fetches_complete_source_and_pages_utf8_safely() {
    let server = MockServer::start().await;
    let source = "abcédef";
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_test/source/main"))
        .respond_with(ResponseTemplate::new(200).set_body_string(source))
        .expect(2)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let first = get_source(
        &mut client,
        "/sap/bc/adt/oo/classes/zcl_test",
        ByteRangeOptions {
            offset: 0,
            limit: Some(4),
        },
    )
    .await
    .unwrap();

    assert_eq!(first.content, "abc");
    assert_eq!(first.start_byte, 0);
    assert_eq!(first.end_byte, 3);
    assert_eq!(first.total_bytes, source.len());
    assert!(first.truncated);
    assert_eq!(first.next_offset, Some(3));

    let second = get_source(
        &mut client,
        "/sap/bc/adt/oo/classes/zcl_test",
        ByteRangeOptions {
            offset: first.next_offset.unwrap(),
            limit: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.content, "édef");
    assert!(!second.truncated);
    server.verify().await;
}

#[tokio::test]
async fn rejects_invalid_source_uris_before_http() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();

    let error = get_source(&mut client, "not-an-adt-uri", ByteRangeOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error, AdtError::InvalidUri(_)));
}

#[tokio::test]
async fn rejects_doubled_source_suffix_and_known_no_source_kinds() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();

    let doubled = get_source(
        &mut client,
        "/sap/bc/adt/oo/classes/zcl_test/source/main",
        ByteRangeOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(doubled, AdtError::DoubledSourceSuffix(_)));

    let domain = get_source(
        &mut client,
        "/sap/bc/adt/ddic/domains/zdomain",
        ByteRangeOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(domain, AdtError::NoSourceForKind { .. }));
}
