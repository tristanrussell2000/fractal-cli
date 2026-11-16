//! Activating several objects in one request.
//!
//! The case that matters is the one SAP gets wrong: a group with one broken
//! object answers "cancelled", yet publishes the other object's source and
//! leaves it listed as inactive. Each object must be judged by its own state.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use fractal::config::{EditPolicy, Profile};
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    client::SapClient,
    editable_source::EditableAdtObjectType,
    group_activation::{GroupActivationRequest, GroupOutcome, activate_adt_sources_together},
};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{body_string_contains, method, path, query_param},
};

const PARENT: &str = "/sap/bc/adt/ddic/ddl/sources/zsample_parent";
const CHILD: &str = "/sap/bc/adt/ddic/ddl/sources/zsample_child";
const PARENT_STAGED: &str =
    "define root view entity ZSAMPLE_PARENT as select from t002 { key spras as Language }";
const CHILD_STAGED: &str =
    "define view entity ZSAMPLE_CHILD as select from t002t { key spras as Language }";

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "100".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        password_command: None,
        edit_packages: None,
        allow_temporary_package: true,
        customer_namespaces: vec!["Z*".to_owned()],
    }
}

fn request() -> GroupActivationRequest {
    GroupActivationRequest {
        objects: vec![
            (
                EditableAdtObjectType::DdlSource,
                "zsample_parent".to_owned(),
            ),
            (EditableAdtObjectType::DdlSource, "zsample_child".to_owned()),
        ],
        transport: None,
    }
}

/// Both listed on the first read; afterwards, only `still_listed`.
#[derive(Clone)]
struct Worklist {
    calls: Arc<AtomicUsize>,
    still_listed: Vec<&'static str>,
}

impl Respond for Worklist {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let listed: Vec<&str> = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![PARENT, CHILD]
        } else {
            self.still_listed.clone()
        };
        let body: String = listed
            .iter()
            .map(|uri| format!("<object uri=\"{uri}\"/>"))
            .collect();
        ResponseTemplate::new(200).set_body_string(format!("<objects>{body}</objects>"))
    }
}

async fn mount_common(server: &MockServer, still_listed: Vec<&'static str>) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "group-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=group; Path=/"),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .respond_with(Worklist {
            calls: Arc::new(AtomicUsize::new(0)),
            still_listed,
        })
        .mount(server)
        .await;
    for (uri, staged) in [(PARENT, PARENT_STAGED), (CHILD, CHILD_STAGED)] {
        Mock::given(method("GET"))
            .and(path(format!("{uri}/source/main")))
            .and(query_param("version", "inactive"))
            .respond_with(ResponseTemplate::new(200).set_body_string(staged))
            .mount(server)
            .await;
    }
}

async fn mount_active(server: &MockServer, uri: &str, source: &'static str) {
    Mock::given(method("GET"))
        .and(path(format!("{uri}/source/main")))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_string(source))
        .mount(server)
        .await;
}

/// The one request naming both objects. No check run may come before it.
async fn mount_activation(server: &MockServer, answer: &str) {
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/activation"))
        .and(query_param("method", "activate"))
        .and(body_string_contains(format!("adtcore:uri=\"{PARENT}\"")))
        .and(body_string_contains(format!("adtcore:uri=\"{CHILD}\"")))
        .respond_with(ResponseTemplate::new(200).set_body_string(answer.to_owned()))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/checkruns"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
}

async fn client(server: &MockServer) -> SapClient {
    SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap()
}

#[tokio::test]
async fn activates_a_pair_together_and_proves_each() {
    let server = MockServer::start().await;
    mount_common(&server, Vec::new()).await;
    mount_activation(
        &server,
        r#"<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist"><chkl:properties activationExecuted="true"/></chkl:messages>"#,
    )
    .await;
    mount_active(&server, PARENT, PARENT_STAGED).await;
    mount_active(&server, CHILD, CHILD_STAGED).await;

    let result = activate_adt_sources_together(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        None,
    )
    .await
    .unwrap();

    let outcomes: Vec<_> = result.members.iter().map(|member| member.outcome).collect();
    assert_eq!(outcomes, [GroupOutcome::Activated, GroupOutcome::Activated]);
    server.verify().await;
}

/// The answer SAP gave live for a pair whose child had an unknown column.
const CANCELLED: &str = r#"<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist">
<chkl:properties checkExecuted="true" activationExecuted="true" generationExecuted="false"/>
<msg objDescr="" type="W" line="0" href=""><shortText><txt>Activation was cancelled.</txt></shortText></msg>
<msg objDescr="DDLS ZSAMPLE_CHILD" type="E" line="1" href="/sap/bc/adt/ddic/ddl/sources/zsample_child"><shortText><txt>The column sptxtx is unknown</txt></shortText></msg>
<msg objDescr="DDLS ZSAMPLE_CHILD was not activated" type="E" line="2" href="/sap/bc/adt/ddic/ddl/sources/zsample_child"><shortText><txt>DDLS ZSAMPLE_CHILD was not activated</txt></shortText></msg>
</chkl:messages>"#;

#[tokio::test]
async fn a_cancelled_group_reports_each_object_by_its_own_state() {
    let server = MockServer::start().await;
    // Both stay listed, but the parent's staged source did go active.
    mount_common(&server, vec![PARENT, CHILD]).await;
    mount_activation(&server, CANCELLED).await;
    mount_active(&server, PARENT, PARENT_STAGED).await;
    mount_active(
        &server,
        CHILD,
        "define view entity ZSAMPLE_CHILD as select from t002t { key spras }",
    )
    .await;

    let error = activate_adt_sources_together(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_activation_incomplete");
    let message = error.to_string();
    assert!(
        message.contains("ZSAMPLE_PARENT published_still_listed"),
        "{message}"
    );
    assert!(message.contains("ZSAMPLE_CHILD not_activated"), "{message}");
    let hint = error.hint().unwrap();
    assert!(hint.contains("The column sptxtx is unknown"), "{hint}");
    server.verify().await;
}

#[tokio::test]
async fn an_object_with_nothing_staged_is_left_out_of_the_request() {
    let server = MockServer::start().await;
    mount_common(&server, Vec::new()).await;
    // The worklist lists the parent (and an unrelated child); ZSAMPLE_OTHER
    // has nothing staged.
    let request = GroupActivationRequest {
        objects: vec![
            (
                EditableAdtObjectType::DdlSource,
                "zsample_parent".to_owned(),
            ),
            (EditableAdtObjectType::DdlSource, "zsample_other".to_owned()),
        ],
        transport: None,
    };
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/activation"))
        .and(body_string_contains(format!("adtcore:uri=\"{PARENT}\"")))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<chkl:messages xmlns:chkl=\"http://www.sap.com/abapxml/checklist\"/>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    mount_active(&server, PARENT, PARENT_STAGED).await;

    let result = activate_adt_sources_together(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.members.len(), 1);
    assert_eq!(result.nothing_to_activate[0].name, "ZSAMPLE_OTHER");
    let sent = server.received_requests().await.unwrap();
    let body = sent
        .iter()
        .find(|request| request.url.path() == "/sap/bc/adt/activation")
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .unwrap();
    assert!(!body.contains("zsample_other"), "{body}");
}

fn journal(dir: &tempfile::TempDir) -> fractal::journal::recorder::Journal {
    fractal::journal::recorder::Journal::with_roots(
        dir.path().join("blobs"),
        dir.path().join("journal/de3"),
        fractal::journal::entry::EntrySystem {
            base_url: "https://sap.example:8001".to_owned(),
            profile: "dev".to_owned(),
            client: "100".to_owned(),
            user: "developer".to_owned(),
        },
    )
}

#[tokio::test]
async fn each_entry_is_written_with_its_group_and_resolved_by_its_own_outcome() {
    let server = MockServer::start().await;
    mount_common(&server, vec![PARENT, CHILD]).await;
    mount_activation(&server, CANCELLED).await;
    mount_active(&server, PARENT, PARENT_STAGED).await;
    mount_active(
        &server,
        CHILD,
        "define view entity ZSAMPLE_CHILD as select from t002t { key spras }",
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);

    let _ = activate_adt_sources_together(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        Some(&journal),
    )
    .await
    .unwrap_err();

    let entries = journal.entries().list().unwrap();
    assert_eq!(entries.len(), 2);
    let group = entries[0].group.clone().expect("written with its group");
    assert!(
        entries
            .iter()
            .all(|entry| entry.group.as_deref() == Some(group.as_str()))
    );
    let statuses: Vec<_> = entries.iter().map(|entry| entry.status.as_str()).collect();
    // The parent's source went active (unverified: still listed); the child's did not.
    assert!(
        statuses.contains(&"unverified") && statuses.contains(&"failed"),
        "{statuses:?}"
    );
}

#[tokio::test]
async fn a_member_that_cannot_be_journaled_leaves_no_entry_pending() {
    // The child's active version cannot be read for its before-image, so the
    // group is refused before anything is activated. The parent's entry,
    // already opened, must record that rather than stay pending.
    let server = MockServer::start().await;
    mount_common(&server, Vec::new()).await;
    mount_active(&server, PARENT, PARENT_STAGED).await;
    Mock::given(method("GET"))
        .and(path(format!("{CHILD}/source/main")))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/activation"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);

    let error = activate_adt_sources_together(
        &mut client(&server).await,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(),
        Some(&journal),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_activation_before_image_unreadable");
    let statuses: Vec<_> = journal
        .entries()
        .list()
        .unwrap()
        .iter()
        .map(|entry| entry.status.as_str())
        .collect();
    assert_eq!(statuses, ["failed"]);
    server.verify().await;
}
