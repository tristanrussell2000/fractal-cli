//! Assessing an undo: which entry, whether it can be acted on, and whether the
//! object still holds what the entry recorded.
//!
//! Nothing here may write. The assertion repeated most often is the negative
//! one — `expect(0)` on every mutating method — because a refusal that still
//! wrote would be the worst possible outcome of a feature whose whole purpose
//! is recovery.

use fractal::config::{EditPolicy, Profile};
use fractal::journal::entry::{EntryObject, EntrySystem, JournalEntry, JournalOperation};
use fractal::journal::recorder::Journal;
use fractal::reportable_error::ReportableError;
use fractal::sap::client::SapClient;
use fractal::sap::object_family::AdtObjectFamily;
use fractal::sap::undo::{UndoError, UndoPlan, plan_activation_undo};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

const CLASS_URI: &str = "/sap/bc/adt/oo/classes/zcl_sample";
const CLASS_SOURCE_URI: &str = "/sap/bc/adt/oo/classes/zcl_sample/source/main";
const DTEL_URI: &str = "/sap/bc/adt/ddic/dataelements/zsample_de";

const PREVIOUS_ACTIVE: &str = "CLASS zcl_sample DEFINITION.\n\" old\nENDCLASS.\n";
const PENDING: &str = "CLASS zcl_sample DEFINITION.\n\" pending\nENDCLASS.\n";
const ACTIVATED: &str = "CLASS zcl_sample DEFINITION.\n\" new\nENDCLASS.\n";

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

fn journal(dir: &tempfile::TempDir) -> Journal {
    Journal::with_roots(
        dir.path().join("blobs"),
        dir.path().join("journal/de3"),
        EntrySystem {
            base_url: "https://sap.example:8001".to_owned(),
            profile: "dev".to_owned(),
            client: "100".to_owned(),
            user: "developer".to_owned(),
        },
    )
}

fn class() -> EntryObject {
    EntryObject {
        object_type: AdtObjectFamily::parse("CLAS").unwrap(),
        name: "ZCL_SAMPLE".to_owned(),
        uri: CLASS_URI.to_owned(),
        source_part: None,
    }
}

fn data_element() -> EntryObject {
    EntryObject {
        object_type: AdtObjectFamily::parse("DTEL").unwrap(),
        name: "ZSAMPLE_DE".to_owned(),
        uri: DTEL_URI.to_owned(),
        source_part: None,
    }
}

/// One recorded activation: it replaced `active_before`, consumed
/// `inactive_before`, and left `active_after` in place.
fn recorded(
    journal: &Journal,
    object: EntryObject,
    active_before: Option<&str>,
    inactive_before: Option<&str>,
    active_after: &str,
) -> JournalEntry {
    let entry = journal
        .begin(
            object,
            JournalOperation::Activate,
            None,
            active_before.map(str::to_owned),
            inactive_before.map(str::to_owned),
        )
        .unwrap();
    journal.succeeded(entry, Some(active_after), None).unwrap()
}

/// Every method that could change something, mounted to fail the test if it is
/// ever called.
async fn forbid_writes(server: &MockServer) {
    for verb in ["PUT", "POST", "DELETE"] {
        Mock::given(method(verb))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(server)
            .await;
    }
}

async fn mount_active_source(server: &MockServer, source: &str) {
    Mock::given(method("GET"))
        .and(path(CLASS_SOURCE_URI))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_string(source.to_owned()))
        .mount(server)
        .await;
}

fn document(version: &str, links: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:type="DTEL/DE" adtcore:version="{version}">{links}<dtel:dataElement xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements"><dtel:typeKind>predefinedAbapType</dtel:typeKind></dtel:dataElement></blue:wbobj>"#
    )
}

const STATES_LINK: &str = r#"<atom:link href="./zsample_de?version=inactive" rel="http://www.sap.com/adt/relations/objectstates" title="Complementary active/inactive version" xmlns:atom="http://www.w3.org/2005/Atom"/>"#;

async fn mount_active_document(server: &MockServer, document: String) {
    Mock::given(method("GET"))
        .and(path(DTEL_URI))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_string(document))
        .mount(server)
        .await;
}

async fn plan(
    server: &MockServer,
    journal: &Journal,
    entry: JournalEntry,
    force: bool,
) -> Result<UndoPlan, UndoError> {
    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    plan_activation_undo(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        entry,
        journal.blobs(),
        force,
    )
    .await
}

#[tokio::test]
async fn an_unchanged_object_can_be_undone_and_the_plan_says_what_it_would_do() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_active_source(&server, ACTIVATED).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(
        &journal,
        class(),
        Some(PREVIOUS_ACTIVE),
        Some(PENDING),
        ACTIVATED,
    );

    let plan = plan(&server, &journal, entry, false).await.unwrap();

    assert!(plan.matches_recorded_state());
    // Step 1 restores the previous active version, step 3 puts back the pending
    // work the activation consumed. Undoing only the first would discard it.
    assert_eq!(plan.restore, PREVIOUS_ACTIVE);
    assert_eq!(plan.restore_inactive.as_deref(), Some(PENDING));
    assert!(plan.overridden.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn an_entry_with_no_pending_work_leaves_the_inactive_layer_alone() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_active_source(&server, ACTIVATED).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(&journal, class(), Some(PREVIOUS_ACTIVE), None, ACTIVATED);

    let plan = plan(&server, &journal, entry, false).await.unwrap();

    // "I had none" is different from "there was empty pending work": step 3
    // must do nothing rather than write an empty version.
    assert_eq!(plan.restore_inactive, None);
    server.verify().await;
}

#[tokio::test]
async fn an_object_that_moved_since_the_activation_is_refused_without_writing() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    // Somebody activated something else afterwards.
    mount_active_source(
        &server,
        "CLASS zcl_sample DEFINITION.\n\" theirs\nENDCLASS.\n",
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(
        &journal,
        class(),
        Some(PREVIOUS_ACTIVE),
        Some(PENDING),
        ACTIVATED,
    );

    let error = plan(&server, &journal, entry, false).await.unwrap_err();

    assert_eq!(error.code(), "undo_stale");
    // A refusal is still an answer: it names the blob holding the content.
    let hint = error.hint().expect("has a hint");
    assert!(hint.contains("blobs/"), "{hint}");
    server.verify().await;
}

#[tokio::test]
async fn force_proceeds_past_a_stale_object_and_records_that_it_did() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_active_source(
        &server,
        "CLASS zcl_sample DEFINITION.\n\" theirs\nENDCLASS.\n",
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(
        &journal,
        class(),
        Some(PREVIOUS_ACTIVE),
        Some(PENDING),
        ACTIVATED,
    );

    let plan = plan(&server, &journal, entry, true).await.unwrap();

    assert!(!plan.matches_recorded_state());
    assert_eq!(
        plan.overridden
            .iter()
            .map(|o| o.as_str())
            .collect::<Vec<_>>(),
        vec!["stale"]
    );
    server.verify().await;
}

#[tokio::test]
async fn a_first_activation_is_never_undone_into_a_delete() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    // No read is mounted: the refusal must come before SAP is touched at all.
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(&journal, class(), None, Some(PENDING), ACTIVATED);

    let error = plan(&server, &journal, entry, true).await.unwrap_err();

    // `--force` was passed and must not help: the invariant is that undo never
    // becomes a path to delete.
    assert_eq!(error.code(), "undo_would_delete");
    server.verify().await;
}

#[tokio::test]
async fn a_delete_entry_is_not_something_undo_reverses() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = journal
        .begin(
            class(),
            JournalOperation::Delete,
            None,
            Some(PREVIOUS_ACTIVE.to_owned()),
            None,
        )
        .unwrap();
    let entry = journal.succeeded(entry, None, None).unwrap();

    let error = plan(&server, &journal, entry, true).await.unwrap_err();

    assert_eq!(error.code(), "undo_unsupported_operation");
    server.verify().await;
}

#[tokio::test]
async fn a_refused_operation_left_nothing_to_undo() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = journal
        .begin(
            class(),
            JournalOperation::Activate,
            None,
            Some(PREVIOUS_ACTIVE.to_owned()),
            Some(PENDING.to_owned()),
        )
        .unwrap();
    let entry = journal.failed(entry).unwrap();

    let error = plan(&server, &journal, entry, false).await.unwrap_err();

    assert_eq!(error.code(), "undo_entry_unresolved");
    server.verify().await;
}

#[tokio::test]
async fn an_unresolved_entry_can_be_forced_because_its_before_image_is_genuine() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_active_source(&server, ACTIVATED).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    // A crash between the two journal writes leaves exactly this.
    let entry = journal
        .begin(
            class(),
            JournalOperation::Activate,
            None,
            Some(PREVIOUS_ACTIVE.to_owned()),
            Some(PENDING.to_owned()),
        )
        .unwrap();

    let plan = plan(&server, &journal, entry, true).await.unwrap();

    // Both refusals apply: it was never resolved, and with no after-image
    // there was nothing to gate on.
    assert_eq!(
        plan.overridden
            .iter()
            .map(|o| o.as_str())
            .collect::<Vec<_>>(),
        vec!["entry_unresolved", "no_after_image"]
    );
    assert_eq!(plan.restore, PREVIOUS_ACTIVE);
    server.verify().await;
}

#[tokio::test]
async fn content_the_sweep_removed_is_refused_rather_than_half_restored() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(
        &journal,
        class(),
        Some(PREVIOUS_ACTIVE),
        Some(PENDING),
        ACTIVATED,
    );
    let hash = entry.active_before.sha256().unwrap().to_owned();
    std::fs::remove_file(journal.blobs().path_of(&hash)).unwrap();

    let error = plan(&server, &journal, entry, true).await.unwrap_err();

    assert_eq!(error.code(), "journal_blob_missing");
    server.verify().await;
}

#[tokio::test]
async fn a_metadata_object_with_pending_work_is_not_reported_as_stale() {
    // The step-5 fix, seen from the gate. SAP adds the complementary-states
    // link to the active document as soon as an inactive version exists, so
    // comparing raw bytes would refuse every object somebody has pending work
    // on — which is exactly when an undo is wanted.
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    // Recorded with no pending work, so no states link at the time.
    let entry = recorded(
        &journal,
        data_element(),
        Some(&document("active", "")),
        None,
        &document("active", ""),
    );
    // Read back now that somebody has staged an edit.
    mount_active_document(&server, document("active", STATES_LINK)).await;

    let plan = plan(&server, &journal, entry, false).await.unwrap();

    assert!(plan.matches_recorded_state());
    server.verify().await;
}

#[tokio::test]
async fn a_metadata_document_that_reports_itself_new_is_not_an_active_version() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(
        &journal,
        data_element(),
        Some(&document("active", "")),
        None,
        &document("active", ""),
    );
    // `?version=active` serves the pending document when there is no active
    // version, so the document's own layer has to decide.
    mount_active_document(&server, document("new", "")).await;

    let error = plan(&server, &journal, entry, false).await.unwrap_err();

    assert_eq!(error.code(), "undo_stale");
    server.verify().await;
}

#[tokio::test]
async fn an_object_outside_the_customer_namespaces_is_refused_before_reading() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(
        &journal,
        EntryObject {
            name: "SAPSAMPLE".to_owned(),
            uri: "/sap/bc/adt/oo/classes/sapsample".to_owned(),
            ..class()
        },
        Some(PREVIOUS_ACTIVE),
        None,
        ACTIVATED,
    );

    let error = plan(&server, &journal, entry, true).await.unwrap_err();

    // The entry's stored URI is not trusted: the name is re-checked, so an undo
    // cannot reach somewhere a forward edit could not.
    assert_eq!(error.code(), "object_outside_customer_namespaces");
    server.verify().await;
}
