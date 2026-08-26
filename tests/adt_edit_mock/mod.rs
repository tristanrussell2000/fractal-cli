//! Mechanical Wiremock setup shared by the ADT mutation suites.
//!
//! Only the session plumbing that every guarded edit performs identically
//! lives here: the discovery/CSRF handshake, the stateful lock and unlock
//! requests, the matchers common to a source read or write, and an ordered
//! response sequence. Each suite keeps its own request ordering, exact write
//! bodies, expected call counts, failure scenarios, and safety assertions,
//! because those are the behavior the suite exists to specify.
//!
//! Helpers that return a `MockBuilder` are deliberate: the caller still writes
//! the assertion that matters to it — the exact body, the response, how many
//! times it may be called — while the session matchers it would otherwise
//! retype are supplied here.
//!
//! Each test binary compiles this module separately and uses a subset of it,
//! so unused items are expected.
#![allow(dead_code)]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use fractal::config::Profile;
use wiremock::{
    Mock, MockBuilder, MockServer, Request, Respond, ResponseTemplate,
    matchers::{header, method, path, query_param, query_param_is_missing},
};

const DISCOVERY_PATH: &str = "/sap/bc/adt/core/discovery";
const STATEFUL_SESSION_HEADER: &str = "x-sap-adt-sessiontype";
const LOCK_RESULT_MEDIA_TYPE: &str =
    "application/vnd.sap.as+xml; charset=UTF-8; dataname=com.sap.adt.lock.Result";

/// The SAP session identity one mutation suite mocks.
///
/// Each suite uses distinct token, cookie, and lock-handle values so that a
/// mock wired to the wrong suite's session fails loudly instead of matching.
pub struct AdtEditSession {
    pub sap_client: &'static str,
    pub csrf_token: &'static str,
    pub session_cookie: &'static str,
    pub object_path: &'static str,
    pub source_path: &'static str,
    pub lock_handle: &'static str,
}

impl AdtEditSession {
    pub fn profile(&self, base_url: String, customer_namespaces: &[&str]) -> Profile {
        Profile {
            base_url,
            client: self.sap_client.to_owned(),
            username: "developer".to_owned(),
            insecure_tls: false,
            customer_namespaces: customer_namespaces
                .iter()
                .map(|namespace| (*namespace).to_owned())
                .collect(),
        }
    }

    /// The discovery handshake that establishes the CSRF token and session cookie.
    pub async fn mount_csrf_session(&self, server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(DISCOVERY_PATH))
            .and(query_param("sap-client", self.sap_client))
            .and(header("x-csrf-token", "Fetch"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-csrf-token", self.csrf_token)
                    .insert_header("set-cookie", format!("{}; Path=/", self.session_cookie)),
            )
            .expect(1)
            .mount(server)
            .await;
    }

    /// A successful stateful MODIFY lock, with or without a transport.
    pub async fn mount_lock(&self, server: &MockServer, transport: Option<&str>) {
        self.lock_request(transport)
            .respond_with(ResponseTemplate::new(200).set_body_string(self.lock_result_body()))
            .expect(1)
            .mount(server)
            .await;
    }

    /// A lock attempt answered by a caller-supplied response, such as a conflict.
    pub async fn mount_lock_response(
        &self,
        server: &MockServer,
        transport: Option<&str>,
        response: ResponseTemplate,
    ) {
        self.lock_request(transport)
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    /// The unlock that follows every acquired lock. `corrNr` is never sent.
    pub async fn mount_unlock(&self, server: &MockServer, response: ResponseTemplate) {
        self.unlock_request()
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    /// The stateful lock request, without a response or expected call count.
    pub fn lock_request(&self, transport: Option<&str>) -> MockBuilder {
        let request = self
            .stateful_session(Mock::given(method("POST")).and(path(self.object_path)))
            .and(query_param("_action", "LOCK"))
            .and(query_param("accessMode", "MODIFY"))
            .and(header("accept", LOCK_RESULT_MEDIA_TYPE));
        with_transport(request, transport)
    }

    /// The stateful unlock request, without a response or expected call count.
    pub fn unlock_request(&self) -> MockBuilder {
        self.stateful_session(Mock::given(method("POST")).and(path(self.object_path)))
            .and(query_param("_action", "UNLOCK"))
            .and(query_param("lockHandle", self.lock_handle))
            .and(query_param_is_missing("corrNr"))
    }

    /// The stateful source PUT. Callers add the exact body they expect written.
    pub fn source_write(&self, transport: Option<&str>) -> MockBuilder {
        let request = self
            .stateful_session(Mock::given(method("PUT")).and(path(self.source_path)))
            .and(query_param("lockHandle", self.lock_handle))
            .and(header("content-type", "text/plain; charset=utf-8"));
        with_transport(request, transport)
    }

    /// A source read of one version. Callers add the stateful-session header
    /// where the read must share the lock's session, and the response.
    pub fn source_read(&self, version: &str) -> MockBuilder {
        Mock::given(method("GET"))
            .and(path(self.source_path))
            .and(query_param("version", version))
            .and(query_param("sap-client", self.sap_client))
    }

    pub fn lock_result_body(&self) -> String {
        format!(
            "<lockResult><LOCK_HANDLE>{}</LOCK_HANDLE></lockResult>",
            self.lock_handle
        )
    }

    fn stateful_session(&self, mock: MockBuilder) -> MockBuilder {
        mock.and(query_param("sap-client", self.sap_client))
            .and(header("x-csrf-token", self.csrf_token))
            .and(header("cookie", self.session_cookie))
            .and(header(STATEFUL_SESSION_HEADER, "stateful"))
    }
}

fn with_transport(mock: MockBuilder, transport: Option<&str>) -> MockBuilder {
    match transport {
        Some(transport) => mock.and(query_param("corrNr", transport)),
        None => mock.and(query_param_is_missing("corrNr")),
    }
}

/// Answers repeated calls to one mock with an ordered list of responses,
/// repeating the last once the list is exhausted.
///
/// Guarded edits read the same source URI more than once — under the lock and
/// again to verify what SAP stored — so a suite needs the second answer to
/// differ from the first.
#[derive(Clone)]
pub struct SequentialResponses {
    calls: Arc<AtomicUsize>,
    responses: Arc<Vec<ResponseTemplate>>,
}

impl SequentialResponses {
    #[must_use]
    pub fn new(responses: Vec<ResponseTemplate>) -> Self {
        assert!(
            !responses.is_empty(),
            "a sequential responder needs at least one response"
        );
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            responses: Arc::new(responses),
        }
    }

    /// Answers each call with the next source body.
    #[must_use]
    pub fn sources(sources: &[&str]) -> Self {
        Self::new(
            sources
                .iter()
                .map(|source| ResponseTemplate::new(200).set_body_bytes(source.as_bytes()))
                .collect(),
        )
    }
}

impl Respond for SequentialResponses {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses[call.min(self.responses.len() - 1)].clone()
    }
}
