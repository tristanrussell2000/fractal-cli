use std::{sync::Arc, time::Duration};

use reqwest::{
    Client, RequestBuilder, Response, StatusCode, Url,
    cookie::Jar,
    header::{HeaderMap, HeaderValue},
};
use serde::Serialize;
use thiserror::Error;

use crate::config::Profile;

const DISCOVERY_PATH: &str = "/sap/bc/adt/core/discovery";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SapErrorKind {
    AuthenticationFailed,
    Forbidden,
    NotFound,
    ServerError,
    Other,
}

impl SapErrorKind {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => "authentication_failed",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::ServerError => "server_error",
            Self::Other => "http_error",
        }
    }

    #[must_use]
    pub const fn hint(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => {
                "Check the selected profile, SAP username, password, and client."
            }
            Self::Forbidden => {
                "The credentials were understood but access was refused; check SAP permissions or CSRF/session state."
            }
            Self::NotFound => {
                "Check the SAP endpoint or ADT object path and the selected environment."
            }
            Self::ServerError => {
                "SAP reported a server-side failure; inspect the SAP message and retry if appropriate."
            }
            Self::Other => "Inspect the SAP message and HTTP status for the cause.",
        }
    }
}

#[derive(Debug, Error)]
pub enum SapError {
    #[error("could not build SAP HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("invalid SAP base URL '{url}': {source}")]
    InvalidUrl {
        url: String,
        source: url::ParseError,
    },
    #[error("could not reach SAP at {url}: {message}")]
    Network { url: String, message: String },
    #[error("SAP returned HTTP {status} from {url}: {message}")]
    Http {
        kind: SapErrorKind,
        status: StatusCode,
        url: String,
        message: String,
    },
}

impl SapError {
    #[must_use]
    pub fn is_csrf_failure(&self) -> bool {
        match self {
            Self::Http {
                status: StatusCode::FORBIDDEN,
                message,
                ..
            } => {
                let message = message.to_ascii_lowercase();
                message.contains("csrf")
                    || message.contains("cross-site request forgery")
                    || message.contains("token validation")
            }
            _ => false,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Client(_) => "client_error",
            Self::InvalidUrl { .. } => "invalid_url",
            Self::Network { .. } => "network_error",
            Self::Http { kind, .. } => kind.code(),
        }
    }

    #[must_use]
    pub const fn hint(&self) -> &'static str {
        match self {
            Self::Client(_) => "The local HTTP client could not be initialized.",
            Self::InvalidUrl { .. } => "Use a complete SAP URL including http:// or https://.",
            Self::Network { .. } => {
                "Check the VPN, hostname, port, and whether the SAP service is reachable."
            }
            Self::Http { kind, .. } => kind.hint(),
        }
    }
}

#[derive(Debug)]
pub struct DiscoveryResult {
    pub url: Url,
    pub status: StatusCode,
    pub csrf_token_received: bool,
}

pub struct SapClient {
    http: Client,
    base_url: Url,
    client_id: String,
    username: String,
    password: String,
    csrf_token: Option<String>,
}

impl SapClient {
    /// Creates an authenticated SAP HTTP client for a saved profile.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile URL is invalid or the HTTP client
    /// cannot be initialized.
    pub fn new(profile: &Profile, password: String) -> Result<Self, SapError> {
        let base_url = Url::parse(profile.base_url.trim_end_matches('/')).map_err(|source| {
            SapError::InvalidUrl {
                url: profile.base_url.clone(),
                source,
            }
        })?;
        let cookie_jar = Arc::new(Jar::default());
        let http = Client::builder()
            .cookie_provider(cookie_jar)
            .danger_accept_invalid_certs(profile.insecure_tls)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(SapError::Client)?;

        Ok(Self {
            http,
            base_url,
            client_id: profile.client.clone(),
            username: profile.username.clone(),
            password,
            csrf_token: None,
        })
    }

    /// Fetches a text response from a SAP path.
    ///
    /// # Errors
    ///
    /// Returns [`SapError`] for URL, network, HTTP, or response-body failures.
    pub async fn get_text(&self, path: &str) -> Result<String, SapError> {
        self.get_text_with_query(path, &[]).await
    }

    /// Fetches a text response with additional query parameters.
    ///
    /// This does not mutate CSRF state and can be called concurrently through
    /// shared references.
    ///
    /// # Errors
    ///
    /// Returns [`SapError`] for URL, network, HTTP, or response-body failures.
    pub async fn get_text_with_query(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<String, SapError> {
        let (_, response) = self.get_read_only(path, query, HeaderMap::new()).await?;
        response.text().await.map_err(|error| SapError::Network {
            url: self.base_url.to_string(),
            message: format!("could not read SAP response body: {error}"),
        })
    }

    /// Fetches response bytes with caller-provided headers.
    ///
    /// This is kept inside the SAP layer for workflows, such as ADT editing,
    /// that must keep a read in the same explicitly stateful session as a lock.
    pub(crate) async fn get_bytes_with_query_and_headers(
        &self,
        path: &str,
        query: &[(&str, &str)],
        headers: HeaderMap,
    ) -> Result<Vec<u8>, SapError> {
        let (_, response) = self.get_read_only(path, query, headers).await?;
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| SapError::Network {
                url: self.base_url.to_string(),
                message: format!("could not read SAP response body: {error}"),
            })
    }

    /// Sends a text PUT request using the SAP session and CSRF token.
    ///
    /// The CSRF token is fetched through ADT discovery when this client does not
    /// already have one. Caller-provided headers are preserved.
    ///
    /// # Errors
    ///
    /// Returns [`SapError`] when the CSRF handshake, request, or response fails.
    pub async fn put_text(
        &mut self,
        path: &str,
        query: &[(&str, &str)],
        body: &str,
        headers: HeaderMap,
    ) -> Result<String, SapError> {
        self.ensure_csrf().await?;
        let url = self.request_url(path, query)?;
        let request = self
            .http
            .put(url.clone())
            .headers(headers)
            .basic_auth(&self.username, Some(&self.password))
            .body(body.to_owned());
        let response = self
            .apply_session_headers(request, true)
            .send()
            .await
            .map_err(|error| SapError::Network {
                url: url.to_string(),
                message: describe_network_error(&error),
            })?;
        if !response.status().is_success() {
            return Err(http_error(url, response).await);
        }
        self.capture_csrf_token(&response);
        response.text().await.map_err(|error| SapError::Network {
            url: self.base_url.to_string(),
            message: format!("could not read SAP response body: {error}"),
        })
    }

    /// Sends a text POST request using the SAP session and CSRF token.
    ///
    /// The CSRF token is fetched through ADT discovery when this client does not
    /// already have one. The optional body is sent as text and caller-provided
    /// headers are preserved.
    ///
    /// # Errors
    ///
    /// Returns [`SapError`] when the CSRF handshake, request, or response fails.
    pub async fn post_text(
        &mut self,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&str>,
        headers: HeaderMap,
    ) -> Result<String, SapError> {
        self.ensure_csrf().await?;
        let url = self.request_url(path, query)?;
        let mut request = self
            .http
            .post(url.clone())
            .headers(headers)
            .basic_auth(&self.username, Some(&self.password));
        if let Some(body) = body {
            request = request.body(body.to_owned());
        }
        let response = self
            .apply_session_headers(request, true)
            .send()
            .await
            .map_err(|error| SapError::Network {
                url: url.to_string(),
                message: describe_network_error(&error),
            })?;
        if !response.status().is_success() {
            return Err(http_error(url, response).await);
        }
        self.capture_csrf_token(&response);
        response.text().await.map_err(|error| SapError::Network {
            url: self.base_url.to_string(),
            message: format!("could not read SAP response body: {error}"),
        })
    }

    /// Ensures that a CSRF token and session are available for read-only POSTs.
    ///
    /// Call this before issuing concurrent requests through
    /// [`Self::post_text_read_only`]. If the client already has a token, this is
    /// a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`SapError`] when the discovery handshake fails.
    pub async fn establish_csrf_session(&mut self) -> Result<(), SapError> {
        self.ensure_csrf().await
    }

    /// Sends a text POST using an already-established CSRF/session state.
    ///
    /// This method does not perform a CSRF handshake or mutate client state, so
    /// it can be used for concurrent read-only POST requests after
    /// [`Self::establish_csrf_session`] has established the session.
    ///
    /// # Errors
    ///
    /// Returns [`SapError`] when the request or response fails.
    pub async fn post_text_read_only(
        &self,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&str>,
        headers: HeaderMap,
    ) -> Result<String, SapError> {
        let url = self.request_url(path, query)?;
        let mut request = self
            .http
            .post(url.clone())
            .headers(headers)
            .basic_auth(&self.username, Some(&self.password));
        if let Some(body) = body {
            request = request.body(body.to_owned());
        }
        let response = self
            .apply_session_headers(request, true)
            .send()
            .await
            .map_err(|error| SapError::Network {
                url: url.to_string(),
                message: describe_network_error(&error),
            })?;
        if !response.status().is_success() {
            return Err(http_error(url, response).await);
        }
        response.text().await.map_err(|error| SapError::Network {
            url: self.base_url.to_string(),
            message: format!("could not read SAP response body: {error}"),
        })
    }

    /// Refreshes the CSRF token and session state through ADT discovery.
    ///
    /// # Errors
    ///
    /// Returns [`SapError`] when the discovery handshake fails.
    pub async fn refresh_csrf(&mut self) -> Result<(), SapError> {
        self.csrf_token = None;
        let mut headers = HeaderMap::new();
        headers.insert("X-CSRF-Token", HeaderValue::from_static("Fetch"));
        let _ = self.get(DISCOVERY_PATH, &[], headers).await?;
        Ok(())
    }

    /// Fetches SAP ADT discovery metadata to verify the connection and credentials.
    ///
    /// # Errors
    ///
    /// Returns [`SapError`] when discovery cannot be reached or SAP rejects the request.
    pub async fn test_connection(&mut self) -> Result<DiscoveryResult, SapError> {
        let mut headers = HeaderMap::new();
        headers.insert("X-CSRF-Token", HeaderValue::from_static("Fetch"));

        let (url, response) = self.get(DISCOVERY_PATH, &[], headers).await?;
        let status = response.status();

        Ok(DiscoveryResult {
            url,
            status,
            csrf_token_received: self.csrf_token.is_some(),
        })
    }

    async fn ensure_csrf(&mut self) -> Result<(), SapError> {
        if self.csrf_token.is_some() {
            return Ok(());
        }
        let mut headers = HeaderMap::new();
        headers.insert("X-CSRF-Token", HeaderValue::from_static("Fetch"));
        let _ = self.get(DISCOVERY_PATH, &[], headers).await?;
        Ok(())
    }

    async fn get_read_only(
        &self,
        path: &str,
        query: &[(&str, &str)],
        headers: HeaderMap,
    ) -> Result<(Url, Response), SapError> {
        let url = self.request_url(path, query)?;
        let request = self
            .http
            .get(url.clone())
            .headers(headers)
            .basic_auth(&self.username, Some(&self.password));
        let response = self
            .apply_session_headers(request, false)
            .send()
            .await
            .map_err(|error| SapError::Network {
                url: url.to_string(),
                message: describe_network_error(&error),
            })?;

        if !response.status().is_success() {
            return Err(http_error(url, response).await);
        }

        Ok((url, response))
    }

    async fn get(
        &mut self,
        path: &str,
        query: &[(&str, &str)],
        headers: HeaderMap,
    ) -> Result<(Url, Response), SapError> {
        let url = self.request_url(path, query)?;
        let request = self
            .http
            .get(url.clone())
            .headers(headers)
            .basic_auth(&self.username, Some(&self.password));
        let response = self
            .apply_session_headers(request, false)
            .send()
            .await
            .map_err(|error| SapError::Network {
                url: url.to_string(),
                message: describe_network_error(&error),
            })?;

        if !response.status().is_success() {
            return Err(http_error(url, response).await);
        }

        self.capture_csrf_token(&response);
        Ok((url, response))
    }

    fn apply_session_headers(&self, mut request: RequestBuilder, mutating: bool) -> RequestBuilder {
        if mutating && let Some(token) = &self.csrf_token {
            request = request.header("X-CSRF-Token", token);
        }
        request
    }

    fn capture_csrf_token(&mut self, response: &Response) {
        if let Some(token) = response.headers().get("x-csrf-token")
            && let Ok(token) = token.to_str()
        {
            self.csrf_token = Some(token.to_owned());
        }
    }

    fn request_url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, SapError> {
        let mut url = self
            .base_url
            .join(path)
            .map_err(|source| SapError::InvalidUrl {
                url: self.base_url.to_string(),
                source,
            })?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("sap-client", &self.client_id);
            for (name, value) in query {
                pairs.append_pair(name, value);
            }
        }
        Ok(url)
    }
}

fn classify_http_status(status: StatusCode) -> SapErrorKind {
    match status {
        StatusCode::UNAUTHORIZED => SapErrorKind::AuthenticationFailed,
        StatusCode::FORBIDDEN => SapErrorKind::Forbidden,
        StatusCode::NOT_FOUND => SapErrorKind::NotFound,
        status if status.is_server_error() => SapErrorKind::ServerError,
        _ => SapErrorKind::Other,
    }
}

async fn http_error(url: Url, response: Response) -> SapError {
    let status = response.status();
    let message = response
        .text()
        .await
        .ok()
        .and_then(|body| extract_sap_message(&body))
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("request failed")
                .to_owned()
        });
    SapError::Http {
        kind: classify_http_status(status),
        status,
        url: url.to_string(),
        message,
    }
}

fn describe_network_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "request timed out; check that the VPN is connected and SAP is reachable".to_owned()
    } else if error.is_builder() {
        error.to_string()
    } else {
        format!("{error}; check the VPN and SAP host")
    }
}

fn extract_sap_message(body: &str) -> Option<String> {
    let message =
        find_xml_text(body, "message").or_else(|| find_xml_text(body, "localizedMessage"))?;
    let message = decode_xml_entities(message.trim());
    (!message.is_empty()).then_some(message)
}

fn find_xml_text<'a>(body: &'a str, element: &str) -> Option<&'a str> {
    let mut search_from = 0;
    while let Some(relative) = body[search_from..].find('<') {
        let open = search_from + relative;
        let after_open = &body[open + 1..];
        let name_end = after_open.find([' ', '>', '/'])?;
        let qualified_name = &after_open[..name_end];
        if qualified_name.rsplit(':').next() != Some(element) {
            search_from = open + 1;
            continue;
        }

        let start = body[open..].find('>')? + open + 1;
        let closing = format!("</{qualified_name}>");
        let close = body[start..].find(&closing)? + start;
        return Some(&body[start..close]);
    }
    None
}

fn decode_xml_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::{SapErrorKind, classify_http_status, decode_xml_entities, extract_sap_message};

    #[test]
    fn extracts_sap_message_from_xml() {
        let body = r#"<error><message>Invalid &amp; expired credentials</message></error>"#;
        assert_eq!(
            extract_sap_message(body).as_deref(),
            Some("Invalid & expired credentials")
        );
    }

    #[test]
    fn extracts_namespaced_sap_message() {
        let body = r#"<a:error><a:message>Not authorized</a:message></a:error>"#;
        assert_eq!(extract_sap_message(body).as_deref(), Some("Not authorized"));
    }

    #[test]
    fn prefers_message_over_localized_message() {
        let body = r#"<error><localizedMessage>Localized</localizedMessage><message>Primary</message></error>"#;
        assert_eq!(extract_sap_message(body).as_deref(), Some("Primary"));
    }

    #[test]
    fn decodes_xml_entities() {
        assert_eq!(decode_xml_entities("a &lt; b &amp; c"), "a < b & c");
    }

    #[test]
    fn classifies_http_statuses_for_agents() {
        assert_eq!(
            classify_http_status(StatusCode::UNAUTHORIZED),
            SapErrorKind::AuthenticationFailed
        );
        assert_eq!(
            classify_http_status(StatusCode::FORBIDDEN),
            SapErrorKind::Forbidden
        );
        assert_eq!(
            classify_http_status(StatusCode::NOT_FOUND),
            SapErrorKind::NotFound
        );
        assert_eq!(
            classify_http_status(StatusCode::INTERNAL_SERVER_ERROR),
            SapErrorKind::ServerError
        );
        assert_eq!(
            classify_http_status(StatusCode::BAD_REQUEST),
            SapErrorKind::Other
        );
    }

    #[test]
    fn error_kinds_have_stable_codes_and_hints() {
        assert_eq!(
            SapErrorKind::AuthenticationFailed.code(),
            "authentication_failed"
        );
        assert!(!SapErrorKind::Forbidden.hint().is_empty());
    }
}
