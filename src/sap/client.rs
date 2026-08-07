use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
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
}

impl SapClient {
    pub fn new(profile: &Profile, password: String) -> Result<Self, SapError> {
        let base_url = Url::parse(profile.base_url.trim_end_matches('/')).map_err(|source| {
            SapError::InvalidUrl {
                url: profile.base_url.clone(),
                source,
            }
        })?;
        let http = Client::builder()
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
        })
    }

    pub async fn test_connection(&self) -> Result<DiscoveryResult, SapError> {
        let mut url =
            self.base_url
                .join(DISCOVERY_PATH)
                .map_err(|source| SapError::InvalidUrl {
                    url: self.base_url.to_string(),
                    source,
                })?;
        url.query_pairs_mut()
            .append_pair("sap-client", &self.sap_client);

        let response = self
            .http
            .get(url.clone())
            .basic_auth(&self.username, Some(&self.password))
            .header("X-CSRF-Token", "Fetch")
            .send()
            .await
            .map_err(|error| SapError::Network {
                url: url.to_string(),
                message: describe_network_error(&error),
            })?;

        let status = response.status();
        if !status.is_success() {
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
            return Err(SapError::Http {
                status,
                url: url.to_string(),
                message,
            });
        }

        let csrf_token_received = response.headers().contains_key("x-csrf-token");
        Ok(DiscoveryResult {
            url,
            status,
            csrf_token_received,
        })
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
