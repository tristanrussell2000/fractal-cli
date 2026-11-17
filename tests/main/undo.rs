//! Assessing an undo: which entry, whether it can be acted on, and whether the
//! object still holds what the entry recorded.
//!
//! Nothing here may write. The assertion repeated most often is the negative
//! one — `expect(0)` on every mutating method — because a refusal that still
//! wrote would be the worst possible outcome of a feature whose whole purpose
//! is recovery.

use fractal::config::{EditPolicy, Profile};
use fractal::journal::entry::{
    ActivationUndoStep, EntryStatus, EntrySystem, JournalEntry, RecordedObject, Stream,
};
use fractal::journal::recorder::{ActivationBefore, AfterImages, Journal};
use fractal::reportable_error::ReportableError;
use fractal::sap::class_include::ClassInclude;
use fractal::sap::client::SapClient;
use fractal::sap::object_family::AdtObjectFamily;
use fractal::sap::undo::{StreamUndo, UndoError, UndoPlan, plan_activation_undo, undo_activation};
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
        dir.path().join("journal/qas"),
        EntrySystem {
            base_url: "https://sap.example:8001".to_owned(),
            profile: "dev".to_owned(),
            client: "100".to_owned(),
            user: "developer".to_owned(),
        },
    )
}

/// What a test records an entry against.
#[derive(Clone)]
struct Target {
    object_type: &'static str,
    name: String,
    uri: String,
}

fn class() -> Target {
    Target {
        object_type: "CLAS",
        name: "ZCL_SAMPLE".to_owned(),
        uri: CLASS_URI.to_owned(),
    }
}

fn data_element() -> Target {
    Target {
        object_type: "DTEL",
        name: "ZSAMPLE_DE".to_owned(),
        uri: DTEL_URI.to_owned(),
    }
}

/// The stream these tests record content in: the only one, or a class's
/// `main`. A class's other includes are recorded as never having existed.
fn primary(target: &Target) -> Stream {
    if target.object_type == "CLAS" {
        Stream::Include(ClassInclude::Main)
    } else {
        Stream::Whole
    }
}

fn object<T>(target: &Target, mut content: impl FnMut(Stream) -> Option<T>) -> RecordedObject<T>
where
    T: Default,
{
    RecordedObject::for_family(
        AdtObjectFamily::parse(target.object_type).unwrap(),
        target.name.clone(),
        target.uri.clone(),
        |stream| content(stream).unwrap_or_default(),
    )
}

/// An activation recorded as `pending`, before it resolved.
fn begun(
    journal: &Journal,
    target: &Target,
    active_before: Option<&str>,
    inactive_before: Option<&str>,
) -> JournalEntry {
    let primary = primary(target);
    journal
        .begin_activation(
            object(target, |stream| {
                (stream == primary).then(|| ActivationBefore {
                    active: active_before.map(str::to_owned),
                    inactive: inactive_before.map(str::to_owned),
                })
            }),
            None,
        )
        .unwrap()
}

/// What every stream held after the activation: the primary one `after`, a
/// class's other includes still absent.
fn after_images(target: &Target, after: &str) -> AfterImages {
    let primary = primary(target);
    match primary {
        Stream::Whole => AfterImages::from([(Stream::Whole, Some(after.to_owned()))]),
        Stream::Include(_) => ClassInclude::ALL
            .into_iter()
            .map(|include| {
                let stream = Stream::Include(include);
                (stream, (stream == primary).then(|| after.to_owned()))
            })
            .collect(),
    }
}

/// One recorded activation: it replaced `active_before`, consumed
/// `inactive_before`, and left `active_after` in place.
fn recorded(
    journal: &Journal,
    target: Target,
    active_before: Option<&str>,
    inactive_before: Option<&str>,
    active_after: &str,
) -> JournalEntry {
    let entry = begun(journal, &target, active_before, inactive_before);
    journal
        .succeeded(entry, &after_images(&target, active_after))
        .unwrap()
}

/// The stream a plan for a [`class`] or [`data_element`] entry is about.
fn primary_undo(plan: &UndoPlan) -> &StreamUndo {
    plan.streams
        .iter()
        .find(|stream| {
            matches!(
                stream.stream,
                Stream::Whole | Stream::Include(ClassInclude::Main)
            )
        })
        .expect("a primary stream")
}

/// How the worklist lists a class whose `main` the caller has staged: by
/// section, never by the bare class URI alone.
fn staged_main() -> String {
    format!("{CLASS_SOURCE_URI}#type=CLAS%2FOSU;name=ZCL_SAMPLE")
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

/// The inactive-objects list, probed to find out whether the caller has pending
/// work that step 1 would write over.
async fn mount_pending_work(server: &MockServer, listed: &str) {
    let body = if listed.is_empty() {
        r#"<?xml version="1.0" encoding="utf-8"?><ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"/>"#.to_owned()
    } else {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects" xmlns:adtcore="http://www.sap.com/adt/core"><ioc:entry><ioc:object><ioc:ref adtcore:uri="{listed}" adtcore:name="X"/></ioc:object></ioc:entry></ioc:inactiveObjects>"#
        )
    };
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

async fn mount_no_pending_work(server: &MockServer) {
    mount_pending_work(server, "").await;
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
    labelled(version, links, "Sample")
}

/// The same document with distinguishable content, so a test can tell the
/// version being restored from the one being replaced.
fn labelled(version: &str, links: &str, label: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:type="DTEL/DE" adtcore:version="{version}">{links}<dtel:dataElement xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements"><dtel:typeKind>predefinedAbapType</dtel:typeKind><dtel:shortFieldLabel>{label}</dtel:shortFieldLabel></dtel:dataElement></blue:wbobj>"#
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
    mount_no_pending_work(&server).await;
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
    assert_eq!(
        primary_undo(&plan).restore.as_deref(),
        Some(PREVIOUS_ACTIVE)
    );
    assert_eq!(
        primary_undo(&plan).restore_inactive.as_deref(),
        Some(PENDING)
    );
    assert!(plan.overridden.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn an_entry_with_no_pending_work_leaves_the_inactive_layer_alone() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_active_source(&server, ACTIVATED).await;
    mount_no_pending_work(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(&journal, class(), Some(PREVIOUS_ACTIVE), None, ACTIVATED);

    let plan = plan(&server, &journal, entry, false).await.unwrap();

    // "I had none" is different from "there was empty pending work": step 3
    // must do nothing rather than write an empty version.
    assert_eq!(primary_undo(&plan).restore_inactive, None);
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
    mount_no_pending_work(&server).await;
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
    mount_no_pending_work(&server).await;
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
    let target = class();
    let primary = primary(&target);
    let entry = journal
        .begin_delete(
            object(&target, |stream| {
                (stream == primary).then(|| Some(PREVIOUS_ACTIVE.to_owned()))
            }),
            None,
            None,
            None,
        )
        .unwrap();
    let entry = journal.succeeded(entry, &AfterImages::new()).unwrap();

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
    let entry = begun(&journal, &class(), Some(PREVIOUS_ACTIVE), Some(PENDING));
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
    mount_no_pending_work(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    // A crash between the two journal writes leaves exactly this.
    let entry = begun(&journal, &class(), Some(PREVIOUS_ACTIVE), Some(PENDING));

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
    assert_eq!(
        primary_undo(&plan).restore.as_deref(),
        Some(PREVIOUS_ACTIVE)
    );
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
    let hash = entry
        .activation_images(primary(&class()))
        .and_then(|images| images.active_before.sha256())
        .unwrap()
        .to_owned();
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
    mount_no_pending_work(&server).await;

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
    mount_no_pending_work(&server).await;

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
        Target {
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

// --- The three write steps ------------------------------------------------

fn session() -> crate::adt_edit_mock::AdtEditSession {
    crate::adt_edit_mock::AdtEditSession {
        sap_client: "100",
        csrf_token: "undo-csrf",
        session_cookie: "SAP_SESSIONID=undo-test",
        object_path: CLASS_URI,
        source_path: CLASS_SOURCE_URI,
        lock_handle: "undo-lock",
    }
}

async fn mount_sequence(server: &MockServer, version: &str, bodies: &[&str]) {
    Mock::given(method("GET"))
        .and(path(CLASS_SOURCE_URI))
        .and(query_param("version", version))
        .respond_with(crate::adt_edit_mock::SequentialResponses::sources(bodies))
        .mount(server)
        .await;
}

/// The list answers three times across a full undo: the plan's pending-work
/// probe, the activation's own probe, and its post-activation check.
async fn mount_inactive_list_sequence(server: &MockServer, listed: &[bool]) {
    for listed in listed {
        let body = if *listed {
            format!(
                r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects" xmlns:adtcore="http://www.sap.com/adt/core"><ioc:entry><ioc:object><ioc:ref adtcore:uri="{CLASS_URI}" adtcore:name="ZCL_SAMPLE"/></ioc:object></ioc:entry></ioc:inactiveObjects>"#
            )
        } else {
            r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"/>"#
                .to_owned()
        };
        Mock::given(method("GET"))
            .and(path("/sap/bc/adt/activation/inactiveobjects"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .up_to_n_times(1)
            .expect(1)
            .mount(server)
            .await;
    }
}

/// Every body PUT to the source, in order.
async fn written_bodies(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.method == wiremock::http::Method::PUT)
        .map(|request| String::from_utf8(request.body.clone()).unwrap())
        .collect()
}

async fn undo(
    server: &MockServer,
    journal: &Journal,
    plan: &UndoPlan,
) -> Result<fractal::sap::undo::UndoOutcome, UndoError> {
    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    undo_activation(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        plan,
        None,
        journal,
    )
    .await
}

#[tokio::test]
async fn undoing_restores_the_previous_active_version_and_the_pending_work() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    // Two write cycles: step 1 and step 3.
    session
        .lock_request(None)
        .respond_with(ResponseTemplate::new(200).set_body_string(session.lock_result_body()))
        .expect(2)
        .mount(&server)
        .await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    session
        .source_write(None)
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/checkruns"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<checkMessageList/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/activation"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<activationResult/>"))
        .expect(1)
        .mount(&server)
        .await;
    // The gate, then the activation's read-back once the previous version is
    // active again. Only two: the undo's activation is not journaled, so it
    // does not take the extra before-image read a journaled one would.
    mount_sequence(&server, "active", &[ACTIVATED, PREVIOUS_ACTIVE]).await;
    // Under step 1's lock, what step 1 stored, the pre-check, under step 3's
    // lock, and what step 3 stored.
    mount_sequence(
        &server,
        "inactive",
        &[
            ACTIVATED,
            PREVIOUS_ACTIVE,
            PREVIOUS_ACTIVE,
            PREVIOUS_ACTIVE,
            ACTIVATED,
        ],
    )
    .await;
    // Nothing pending, then the restored version pending, then activated.
    mount_inactive_list_sequence(&server, &[false, true, false]).await;

    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = recorded(
        &journal,
        class(),
        Some(PREVIOUS_ACTIVE),
        Some(PENDING),
        ACTIVATED,
    );
    let entry_id = entry.id.clone();
    let plan = plan(&server, &journal, entry, false).await.unwrap();

    let outcome = undo(&server, &journal, &plan).await.unwrap();

    // Step 1 writes the previous active version, step 3 puts the pending work
    // back. Getting these the wrong way round would leave the object holding
    // what the undo was meant to remove.
    assert_eq!(
        written_bodies(&server).await,
        vec![PREVIOUS_ACTIVE, PENDING]
    );
    assert_eq!(outcome.steps_run.len(), 3);
    assert_eq!(outcome.inactive_restored, vec![primary(&class())]);
    assert!(!outcome.still_locked);

    // The entry records how far it got, so an interrupted rerun resumes, and
    // it is now marked as reversed.
    let entry = journal.entries().find(&entry_id).unwrap();
    assert_eq!(
        entry.undo_progress(),
        Some(ActivationUndoStep::RestoredInactive)
    );
    assert_eq!(entry.status, EntryStatus::Undone);

    // The undo writes no entry of its own: one entry per logical change, not a
    // chain of undos. Its after-image is still the record of what was active
    // before the undo, so nothing is lost by not writing one.
    assert_eq!(journal.entries().list().unwrap().len(), 1);
    assert_eq!(
        entry
            .activation_images(primary(&class()))
            .and_then(|images| images.active_after.as_ref())
            .and_then(|after| after.sha256()),
        Some(fractal::source_change::source_sha256(ACTIVATED).as_str())
    );
    server.verify().await;
}

#[tokio::test]
async fn an_entry_that_was_already_undone_says_so_rather_than_calling_it_stale() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let mut entry = recorded(
        &journal,
        class(),
        Some(PREVIOUS_ACTIVE),
        Some(PENDING),
        ACTIVATED,
    );
    entry.undone();
    journal.entries().update(&entry).unwrap();

    let error = plan(&server, &journal, entry, false).await.unwrap_err();

    // An undone entry is stale by construction — the object no longer holds its
    // after-image — so reporting staleness would be true and useless.
    assert_eq!(error.code(), "undo_already_undone");
    let hint = error.hint().expect("has a hint");
    assert!(hint.contains("redo"), "{hint}");
    server.verify().await;
}

#[tokio::test]
async fn an_interrupted_undo_resumes_at_the_step_it_reached() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    session.mount_lock(&server, None).await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    session
        .source_write(None)
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // The activation must not run again: it already did.
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/activation"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    mount_sequence(&server, "active", &[PREVIOUS_ACTIVE]).await;
    mount_sequence(&server, "inactive", &[PREVIOUS_ACTIVE, PENDING]).await;
    // A resumed undo must not probe for pending work: what it would find is the
    // inactive version its own earlier run left behind.
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/activation/inactiveobjects"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let mut entry = recorded(
        &journal,
        class(),
        Some(PREVIOUS_ACTIVE),
        Some(PENDING),
        PREVIOUS_ACTIVE,
    );
    // A run that wrote and activated, then died before restoring the pending
    // work — the step whose omission is silent.
    entry.record_undo_step(ActivationUndoStep::Activated);
    journal.entries().update(&entry).unwrap();

    let plan = plan(&server, &journal, entry, false).await.unwrap();
    let outcome = undo(&server, &journal, &plan).await.unwrap();

    assert_eq!(
        outcome.steps_run,
        vec![ActivationUndoStep::RestoredInactive]
    );
    assert_eq!(written_bodies(&server).await, vec![PENDING]);
    server.verify().await;
}

#[tokio::test]
async fn pending_work_that_the_undo_would_publish_or_restore_is_not_a_refusal() {
    // Running the same undo twice. The inactive layer holds the version the
    // first run restored, which is exactly what a second run would restore, so
    // there is nothing to lose. Refusing on "there is pending work" alone would
    // make an undo impossible to repeat.
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_active_source(&server, ACTIVATED).await;
    mount_pending_work(&server, &staged_main()).await;
    Mock::given(method("GET"))
        .and(path(CLASS_SOURCE_URI))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PENDING))
        .mount(&server)
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

    let plan = plan(&server, &journal, entry, false).await.unwrap();

    assert!(!plan.pending_work_at_risk);
    server.verify().await;
}

#[tokio::test]
async fn pending_work_the_undo_is_about_to_activate_is_not_a_refusal() {
    // The other half: pending work identical to the version step 1 writes and
    // step 2 activates, which the undo publishes rather than destroys.
    // Comparing only against what step 3 restores would refuse this one.
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_active_source(&server, ACTIVATED).await;
    mount_pending_work(&server, &staged_main()).await;
    Mock::given(method("GET"))
        .and(path(CLASS_SOURCE_URI))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PREVIOUS_ACTIVE))
        .mount(&server)
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

    let plan = plan(&server, &journal, entry, false).await.unwrap();

    assert!(!plan.pending_work_at_risk);
    server.verify().await;
}

#[tokio::test]
async fn pending_work_staged_since_the_activation_is_not_written_over() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_active_source(&server, ACTIVATED).await;
    // Somebody has staged an edit since, and it is not the version this entry
    // recorded: step 1 would write over it and step 3 would put theirs back.
    mount_pending_work(&server, &staged_main()).await;
    Mock::given(method("GET"))
        .and(path(CLASS_SOURCE_URI))
        .and(query_param("version", "inactive"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("CLASS zcl_sample DEFINITION.\n\" theirs\nENDCLASS.\n"),
        )
        .mount(&server)
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

    assert_eq!(error.code(), "undo_pending_work");
    server.verify().await;
}

#[tokio::test]
async fn undoing_a_metadata_activation_restores_the_document_and_the_pending_one() {
    // The same three steps, through the metadata write path: the whole document
    // is the object, so there is no source to write and no syntax pre-check.
    let server = MockServer::start().await;
    let session = crate::adt_edit_mock::AdtEditSession {
        object_path: DTEL_URI,
        source_path: "",
        ..session()
    };
    session.mount_csrf_session(&server).await;
    session
        .lock_request(None)
        .respond_with(ResponseTemplate::new(200).set_body_string(session.lock_result_body()))
        .expect(2)
        .mount(&server)
        .await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(DTEL_URI))
        .and(query_param("lockHandle", "undo-lock"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/activation"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist"><chkl:properties activationExecuted="true"/></chkl:messages>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    // The write path's own reads: before and after each of its two writes,
    // both naming the inactive layer, which is the one a write lands in.
    Mock::given(method("GET"))
        .and(path(DTEL_URI))
        .and(query_param("version", "inactive"))
        .respond_with(crate::adt_edit_mock::SequentialResponses::sources(&[
            &labelled("inactive", STATES_LINK, "two"),
            &labelled("inactive", STATES_LINK, "one"),
            &labelled("inactive", STATES_LINK, "one"),
            &labelled("inactive", STATES_LINK, "two"),
        ]))
        .mount(&server)
        .await;
    // The gate, then the activation's read-back.
    Mock::given(method("GET"))
        .and(path(DTEL_URI))
        .and(query_param("version", "active"))
        .respond_with(crate::adt_edit_mock::SequentialResponses::sources(&[
            &labelled("active", STATES_LINK, "two"),
            &labelled("active", "", "one"),
        ]))
        .mount(&server)
        .await;
    // No pending work, then the restored document pending, then activated.
    for listed in [false, true, false] {
        let body = if listed {
            format!(
                r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects" xmlns:adtcore="http://www.sap.com/adt/core"><ioc:entry><ioc:object><ioc:ref adtcore:uri="{DTEL_URI}" adtcore:name="ZSAMPLE_DE"/></ioc:object></ioc:entry></ioc:inactiveObjects>"#
            )
        } else {
            r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects"/>"#
                .to_owned()
        };
        Mock::given(method("GET"))
            .and(path("/sap/bc/adt/activation/inactiveobjects"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
    }

    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    // The journal holds canonical documents, so the entry's images have no
    // links even though every read from SAP does.
    let previous = labelled("active", "", "one");
    let pending = labelled("inactive", "", "two");
    let entry = recorded(
        &journal,
        data_element(),
        Some(&previous),
        Some(&pending),
        &labelled("active", "", "two"),
    );
    let entry_id = entry.id.clone();
    let plan = plan(&server, &journal, entry, false).await.unwrap();

    let outcome = undo(&server, &journal, &plan).await.unwrap();

    // What gets written back is the canonical document the journal stored, with
    // no `atom:link` in it: SAP regenerates those itself.
    let written = written_bodies(&server).await;
    assert_eq!(written.len(), 2);
    assert!(
        written.iter().all(|body| !body.contains("atom:link")),
        "{written:?}"
    );
    assert_eq!(written, vec![previous, pending]);
    assert_eq!(outcome.steps_run.len(), 3);

    let entry = journal.entries().find(&entry_id).unwrap();
    assert_eq!(entry.status, EntryStatus::Undone);
    assert_eq!(journal.entries().list().unwrap().len(), 1);
    server.verify().await;
}

// --- Classes: one entry, every include --------------------------------------

const LOCAL_TYPES_URI: &str = "/sap/bc/adt/oo/classes/zcl_sample/includes/implementations";
const LOCAL_BEFORE: &str = "CLASS lhc_sample DEFINITION.\n\" before\nENDCLASS.\n";
const LOCAL_STAGED: &str = "CLASS lhc_sample DEFINITION.\n\" staged\nENDCLASS.\n";

/// A class activation that changed only its local types: `main` stayed as it
/// was, `implementations` went from `LOCAL_BEFORE` to `LOCAL_STAGED`.
fn local_types_activation(journal: &Journal) -> JournalEntry {
    let target = class();
    let entry = journal
        .begin_activation(
            object(&target, |stream| match stream {
                Stream::Include(ClassInclude::Main) => Some(ActivationBefore {
                    active: Some(PREVIOUS_ACTIVE.to_owned()),
                    inactive: None,
                }),
                Stream::Include(ClassInclude::Implementations) => Some(ActivationBefore {
                    active: Some(LOCAL_BEFORE.to_owned()),
                    inactive: Some(LOCAL_STAGED.to_owned()),
                }),
                _ => None,
            }),
            None,
        )
        .unwrap();
    let after: AfterImages = ClassInclude::ALL
        .into_iter()
        .map(|include| {
            let content = match include {
                ClassInclude::Main => Some(PREVIOUS_ACTIVE.to_owned()),
                ClassInclude::Implementations => Some(LOCAL_STAGED.to_owned()),
                _ => None,
            };
            (Stream::Include(include), content)
        })
        .collect();
    journal.succeeded(entry, &after).unwrap()
}

async fn mount_local_types(server: &MockServer, version: &str, bodies: &[&str]) {
    Mock::given(method("GET"))
        .and(path(LOCAL_TYPES_URI))
        .and(query_param("version", version))
        .respond_with(crate::adt_edit_mock::SequentialResponses::sources(bodies))
        .mount(server)
        .await;
}

#[tokio::test]
async fn undoing_a_class_activation_restores_the_include_it_published() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    // `main` in step 1, then the include in steps 1 and 3.
    session
        .lock_request(None)
        .respond_with(ResponseTemplate::new(200).set_body_string(session.lock_result_body()))
        .expect(3)
        .mount(&server)
        .await;
    session
        .unlock_request()
        .respond_with(ResponseTemplate::new(200))
        .expect(3)
        .mount(&server)
        .await;
    // `main` already holds what it held before, so nothing is written to it.
    session
        .source_write(None)
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(LOCAL_TYPES_URI))
        .and(query_param("lockHandle", session.lock_handle))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/checkruns"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<checkMessageList/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/activation"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<activationResult/>"))
        .expect(1)
        .mount(&server)
        .await;
    mount_sequence(&server, "active", &[PREVIOUS_ACTIVE]).await;
    mount_sequence(&server, "inactive", &[PREVIOUS_ACTIVE]).await;
    mount_local_types(&server, "active", &[LOCAL_STAGED]).await;
    // Under step 1's lock, what it stored, under step 3's lock, what it stored.
    mount_local_types(
        &server,
        "inactive",
        &[LOCAL_STAGED, LOCAL_BEFORE, LOCAL_BEFORE, LOCAL_STAGED],
    )
    .await;
    mount_inactive_list_sequence(&server, &[false, true, false]).await;

    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = local_types_activation(&journal);
    let plan = plan(&server, &journal, entry, false).await.unwrap();
    let outcome = undo(&server, &journal, &plan).await.unwrap();

    assert_eq!(
        written_bodies(&server).await,
        vec![LOCAL_BEFORE, LOCAL_STAGED]
    );
    assert_eq!(
        outcome.inactive_restored,
        vec![Stream::Include(ClassInclude::Implementations)]
    );
    server.verify().await;
}

#[tokio::test]
async fn a_class_whose_local_types_moved_since_is_stale() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_sequence(&server, "active", &[PREVIOUS_ACTIVE]).await;
    mount_local_types(
        &server,
        "active",
        &["CLASS lhc_sample DEFINITION.\n\" theirs\nENDCLASS.\n"],
    )
    .await;
    mount_no_pending_work(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = local_types_activation(&journal);

    let error = plan(&server, &journal, entry, false).await.unwrap_err();

    assert_eq!(error.code(), "undo_stale");
    assert!(
        error.hint().unwrap().contains("changed: implementations"),
        "{:?}",
        error.hint()
    );
    server.verify().await;
}

/// Step 2 activates the class, which publishes every include the caller has
/// staged. Pending work in an include the undo never touches would be
/// published along with it, so it is a refusal like any other.
#[tokio::test]
async fn pending_work_in_an_include_the_undo_does_not_touch_is_at_risk() {
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    mount_sequence(&server, "active", &[PREVIOUS_ACTIVE]).await;
    mount_local_types(&server, "active", &[LOCAL_STAGED]).await;
    mount_pending_work(
        &server,
        "/sap/bc/adt/oo/classes/zcl_sample/includes/definitions",
    )
    .await;
    Mock::given(method("GET"))
        .and(path(
            "/sap/bc/adt/oo/classes/zcl_sample/includes/definitions",
        ))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_string("TYPES ty_half_done TYPE i."))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let journal = journal(&dir);
    let entry = local_types_activation(&journal);

    let error = plan(&server, &journal, entry, false).await.unwrap_err();

    assert_eq!(error.code(), "undo_pending_work");
    server.verify().await;
}

#[tokio::test]
async fn an_unreadable_active_version_is_an_error_not_an_absence() {
    // A 500 is not "there is no active version": read as one, the object
    // would look stale or, for a first activation, fresh.
    let server = MockServer::start().await;
    forbid_writes(&server).await;
    Mock::given(method("GET"))
        .and(path(CLASS_SOURCE_URI))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    mount_no_pending_work(&server).await;
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

    assert_eq!(error.code(), "undo_current_state_unreadable");
    assert!(error.hint().unwrap().starts_with("Nothing was written."));
    server.verify().await;
}
