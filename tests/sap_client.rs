use fractal::{
    config::Profile,
    sap::client::{SapClient, SapClientError},
};
use reqwest::header::{HeaderMap, HeaderValue};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{basic_auth, body_string, header, method, path, query_param},
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
async fn discovery_request_contains_sap_authentication_and_csrf_fetch_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(query_param("sap-client", "903"))
        .and(basic_auth("developer", "password"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "mock-csrf-token")
                .insert_header("set-cookie", "SAP_SESSIONID=first; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = client.test_connection().await.unwrap();

    assert_eq!(result.status.as_u16(), 200);
    assert!(result.csrf_token_received);
    server.verify().await;
}

#[tokio::test]
async fn later_requests_reuse_the_session_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "mock-csrf-token")
                .insert_header("set-cookie", "SAP_SESSIONID=first; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/test-endpoint"))
        .and(header("cookie", "SAP_SESSIONID=first"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    client.test_connection().await.unwrap();
    assert_eq!(
        client.get_text("/sap/bc/adt/test-endpoint").await.unwrap(),
        "ok"
    );

    server.verify().await;
}

#[tokio::test]
async fn post_text_fetches_csrf_and_reuses_session_state() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "mock-csrf-token")
                .insert_header("set-cookie", "SAP_SESSIONID=post-test; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/repository/nodestructure"))
        .and(query_param("parent_name", "ZAPP"))
        .and(query_param("sap-client", "903"))
        .and(header("x-csrf-token", "mock-csrf-token"))
        .and(header("cookie", "SAP_SESSIONID=post-test"))
        .and(basic_auth("developer", "password"))
        .and(body_string("request-body"))
        .respond_with(ResponseTemplate::new(200).set_body_string("response-body"))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", HeaderValue::from_static("text/plain"));
    let result = client
        .post_text(
            "/sap/bc/adt/repository/nodestructure",
            &[("parent_name", "ZAPP")],
            Some("request-body"),
            headers,
        )
        .await
        .unwrap();

    assert_eq!(result, "response-body");
    server.verify().await;
}

#[test]
fn csrf_failures_are_distinguished_from_regular_forbidden_errors() {
    let csrf = SapClientError::Http {
        kind: fractal::sap::client::SapHttpErrorKind::Forbidden,
        status: reqwest::StatusCode::FORBIDDEN,
        url: "http://sap".to_owned(),
        message: "CSRF token validation failed".to_owned(),
    };
    let forbidden = SapClientError::Http {
        kind: fractal::sap::client::SapHttpErrorKind::Forbidden,
        status: reqwest::StatusCode::FORBIDDEN,
        url: "http://sap".to_owned(),
        message: "User is not authorized".to_owned(),
    };
    assert!(csrf.is_csrf_failure());
    assert!(!forbidden.is_csrf_failure());
}

#[tokio::test]
async fn discovery_request_returns_sap_xml_error_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(ResponseTemplate::new(401).set_body_string(
            r#"<a:error><a:message>Invalid &amp; expired credentials</a:message></a:error>"#,
        ))
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "wrong-password".to_owned()).unwrap();
    let error = client.test_connection().await.unwrap_err();

    match error {
        SapClientError::Http {
            kind,
            status,
            message,
            ..
        } => {
            assert_eq!(kind.code(), "authentication_failed");
            assert_eq!(status.as_u16(), 401);
            assert_eq!(message, "Invalid & expired credentials");
        }
        other => panic!("expected HTTP error, got {other:?}"),
    }
}

#[tokio::test]
async fn discovery_http_statuses_are_classified() {
    for (status, expected_code) in [
        (401, "authentication_failed"),
        (403, "forbidden"),
        (404, "not_found"),
        (500, "server_error"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/sap/bc/adt/core/discovery"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;

        let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
        let error = client.test_connection().await.unwrap_err();

        match error {
            SapClientError::Http {
                kind,
                status: actual,
                ..
            } => {
                assert_eq!(actual.as_u16(), status);
                assert_eq!(kind.code(), expected_code);
                assert!(!kind.hint().is_empty());
            }
            other => panic!("expected HTTP error, got {other:?}"),
        }
    }
}
