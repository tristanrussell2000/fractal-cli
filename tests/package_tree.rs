use fractal::{
    config::Profile,
    sap::{client::SapClient, package::get_package_tree},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "903".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*", "Y*"].into_iter().map(str::to_owned).collect(),
    }
}

fn nodestructure_response(package: &str, child: Option<&str>) -> String {
    let child_xml = child
        .map(|name| {
            format!(r#"<NODE OBJECT_TYPE="DEVC/K" OBJECT_NAME="{name}" DESCRIPTION="Child"/>"#)
        })
        .unwrap_or_default();
    format!(
        r#"<response><TREE_CONTENT>{child_xml}<NODE OBJECT_TYPE="CLAS/OC" OBJECT_NAME="ZCL_{package}" OBJECT_URI="/class/{package}"/><NODE OBJECT_TYPE="TABL/DT" OBJECT_NAME="ZT_{package}" OBJECT_URI="/table/{package}"/></TREE_CONTENT></response>"#
    )
}

#[tokio::test]
async fn recursively_walks_package_tree_and_counts_kinds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-csrf-token", "csrf"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/repository/nodestructure"))
        .and(query_param("parent_name", "ZAPP"))
        .and(query_param("parent_type", "DEVC/K"))
        .and(header("x-csrf-token", "csrf"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(nodestructure_response("ZAPP", Some("ZSUB"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/repository/nodestructure"))
        .and(query_param("parent_name", "ZSUB"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(nodestructure_response("ZSUB", None)),
        )
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let tree = get_package_tree(&mut client, "zapp", true).await.unwrap();

    assert_eq!(tree.root, "ZAPP");
    assert_eq!(tree.packages.len(), 2);
    assert_eq!(tree.kinds.get("CLAS"), Some(&2));
    assert_eq!(tree.kinds.get("TABL"), Some(&2));
    assert!(tree.packages_failed.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn non_recursive_tree_does_not_fetch_child_packages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-csrf-token", "csrf"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/repository/nodestructure"))
        .and(query_param("parent_name", "ZAPP"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(nodestructure_response("ZAPP", Some("ZSUB"))),
        )
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let tree = get_package_tree(&mut client, "ZAPP", false).await.unwrap();

    assert_eq!(tree.packages.len(), 1);
    assert!(tree.packages_failed.is_empty());
    server.verify().await;
}
