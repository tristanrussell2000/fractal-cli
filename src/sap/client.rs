use std::{sync::Arc, time::Duration};

use reqwest::{
    Client, RequestBuilder, Response, StatusCode, Url,
    cookie::Jar,
    header::{HeaderMap, HeaderValue},
};
use thiserror::Error;

use crate::config::Profile;

const DISCOVERY_PATH: &str = "/sap/bc/adt/core/discovery";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
        status: StatusCode,
        url: String,
        message: String,
    },
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
    sap_client: String,
    username: String,
    password: String,
    csrf_token: Option<String>,
}

impl SapClient {
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
            sap_client: profile.client.clone(),
            username: profile.username.clone(),
            password,
            csrf_token: None,
        })
    }

    pub async fn get_text(&mut self, path: &str) -> Result<String, SapError> {
        let (_, response) = self.get(path, HeaderMap::new()).await?;
        response.text().await.map_err(|error| SapError::Network {
            url: self.base_url.to_string(),
            message: format!("could not read SAP response body: {error}"),
        })
    }

    pub async fn test_connection(&mut self) -> Result<DiscoveryResult, SapError> {
        let mut headers = HeaderMap::new();
        headers.insert("X-CSRF-Token", HeaderValue::from_static("Fetch"));

        let (url, response) = self.get(DISCOVERY_PATH, headers).await?;
        let status = response.status();

        Ok(DiscoveryResult {
            url,
            status,
            csrf_token_received: self.csrf_token.is_some(),
        })
    }

    async fn get(&mut self, path: &str, headers: HeaderMap) -> Result<(Url, Response), SapError> {
        let url = self.request_url(path)?;
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
        if mutating {
            if let Some(token) = &self.csrf_token {
                request = request.header("X-CSRF-Token", token);
            }
        }
        request
    }

    fn capture_csrf_token(&mut self, response: &Response) {
        if let Some(token) = response.headers().get("x-csrf-token") {
            if let Ok(token) = token.to_str() {
                self.csrf_token = Some(token.to_owned());
            }
        }
    }

    fn request_url(&self, path: &str) -> Result<Url, SapError> {
        let mut url = self
            .base_url
            .join(path)
            .map_err(|source| SapError::InvalidUrl {
                url: self.base_url.to_string(),
                source,
            })?;
        url.query_pairs_mut()
            .append_pair("sap-client", &self.sap_client);
        Ok(url)
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
        format!("{}; check the VPN and SAP host", error)
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
        let name_end = after_open
            .find(|character: char| character == ' ' || character == '>' || character == '/')?;
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
    use super::{decode_xml_entities, extract_sap_message};

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
}
