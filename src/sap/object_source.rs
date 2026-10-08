//! Raw source and metadata-XML retrieval with local byte paging.
//!
//! `get_source` and `get_xml` share one module because they are the same
//! fetch-then-page shape over different URIs; splitting them would duplicate
//! the paging abstraction for no gain.

use thiserror::Error;

use super::{
    adt_object_uri::{
        AdtObjectUriError, SOURCE_SUFFIX, validate_adt_object_uri, validate_source_object_uri,
    },
    adt_response::{AdtResponseParseError, parse_adt_document},
    adt_version::AdtVersion,
    client::{SapClient, SapClientError},
    find_non_empty_attribute,
    metadata_document::{declared_version, document_version, strip_navigation_links},
};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::source_change::source_sha256;
use crate::suggested_command;

/// A failure while retrieving object source or metadata XML.
#[derive(Debug, Error)]
pub enum ObjectSourceError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error(transparent)]
    Uri(#[from] AdtObjectUriError),
    #[error("{kind} objects do not have an ABAP source view")]
    NoSourceForKind { kind: String, uri: String },
    #[error("could not preserve valid UTF-8 while paging source: {0}")]
    Encoding(String),
    #[error("could not parse the object document: {0}")]
    Parse(#[from] AdtResponseParseError),
}

impl ObjectSourceError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            _ => None,
        }
    }
}

impl ReportableError for ObjectSourceError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Uri(error) => error.code(),
            Self::NoSourceForKind { .. } => "no_source_for_kind",
            Self::Encoding(_) => "source_encoding_error",
            Self::Parse(error) => error.code(),
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Sap(error) => error.hint()?,
            Self::Uri(error) => error.hint()?,
            Self::NoSourceForKind { .. } => {
                "Use `fractal object xml` to retrieve metadata for this object.".to_owned()
            }
            Self::Encoding(_) => {
                "The source response could not be converted into a safe UTF-8 page; retry without paging or report the object URI."
                    .to_owned()
            }
            Self::Parse(error) => error.hint()?,
        })
    }

    /// A read-only command that diagnoses this failure, if one exists.
    fn suggested_command(&self) -> Option<String> {
        match self {
            // The object exists but has no source view; its metadata does.
            Self::NoSourceForKind { uri, .. } => Some(suggested_command::object_xml(uri)),
            Self::Sap(error) => error.suggested_command(),
            Self::Uri(_) | Self::Encoding(_) | Self::Parse(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ByteRangeOptions {
    pub offset: usize,
    pub limit: Option<usize>,
}

/// A metadata document plus the layer it declared itself to be.
///
/// Separate from [`ByteRangeResult`] because only an XML document carries a
/// layer marker; ABAP source does not.
#[derive(Debug, Clone)]
pub struct XmlReadResult {
    pub page: ByteRangeResult,
    pub declared_version: Option<String>,
    /// A token for `set-xml --expected-sha256`, **not** a checksum of
    /// [`Self::page`].
    ///
    /// Taken over the whole document with its `atom:link` elements removed,
    /// which is what every write and journal path hashes. Over the page it
    /// would be useless the moment a caller paged; over the raw document it
    /// would move whenever somebody staged or discarded pending work, because
    /// ADT rewrites link etags for that.
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ByteRangeResult {
    pub start_byte: usize,
    pub end_byte: usize,
    pub total_bytes: usize,
    pub truncated: bool,
    pub next_offset: Option<usize>,
    pub content: String,
}

/// Fetches an ADT object's complete source and returns a locally paged byte range.
///
/// The HTTP response is fetched in full; `offset` and `limit` are applied locally.
/// Byte boundaries are adjusted so returned text remains valid UTF-8.
///
/// # Errors
///
/// Returns [`ObjectSourceError`] for invalid or unsupported object URIs, SAP request
/// failures, or malformed source responses.
pub async fn get_source(
    sap: &SapClient,
    uri: &str,
    version: AdtVersion,
    options: ByteRangeOptions,
) -> Result<ByteRangeResult, ObjectSourceError> {
    validate_source_object_uri(uri)?;
    let kind = no_source_kind(uri);
    if let Some(kind) = kind {
        return Err(ObjectSourceError::NoSourceForKind {
            kind: kind.to_owned(),
            uri: uri.to_owned(),
        });
    }

    let source_uri = format!("{}{}", uri.trim_end_matches('/'), SOURCE_SUFFIX);
    let source = sap
        .get_text_with_query(&source_uri, &[("version", version.as_str())])
        .await?;
    page_text(&source, options)
}

/// Fetches raw ADT metadata XML for an object URI.
///
/// # Errors
///
/// Returns [`ObjectSourceError::InvalidUri`] for a non-ADT URI or the underlying SAP
/// error when the metadata request fails.
pub async fn get_xml(
    sap: &mut SapClient,
    uri: &str,
    version: AdtVersion,
    options: ByteRangeOptions,
) -> Result<XmlReadResult, ObjectSourceError> {
    validate_adt_object_uri(uri)?;
    let xml = sap
        .get_text_with_query(uri, &[("version", version.as_str())])
        .await?;
    // From the whole document, before paging: a short page still has to report
    // which version it came from. An unparseable response is not a failure here
    // — this command returns whatever ADT served — it just has no version.
    let declared_version = document_version(&xml).ok().flatten();
    let sha256 = source_sha256(&strip_navigation_links(&xml));
    Ok(XmlReadResult {
        page: page_text(&xml, options)?,
        declared_version,
        sha256,
    })
}

fn page_text(text: &str, options: ByteRangeOptions) -> Result<ByteRangeResult, ObjectSourceError> {
    let bytes = text.as_bytes();
    let total_bytes = bytes.len();
    let start_byte = utf8_safe_start(bytes, options.offset.min(total_bytes));
    let requested_end = options.limit.map_or(total_bytes, |limit| {
        start_byte.saturating_add(limit).min(total_bytes)
    });
    let end_byte = utf8_safe_end(bytes, requested_end).max(start_byte);
    let content = std::str::from_utf8(&bytes[start_byte..end_byte])
        .map_err(|error| ObjectSourceError::Encoding(error.to_string()))?
        .to_owned();
    let truncated = end_byte < total_bytes;

    Ok(ByteRangeResult {
        start_byte,
        end_byte,
        total_bytes,
        truncated,
        next_offset: truncated.then_some(end_byte),
        content,
    })
}

fn no_source_kind(uri: &str) -> Option<&'static str> {
    let uri = uri.to_ascii_lowercase();
    if uri.contains("/ddic/dataelements/") {
        Some("DTEL")
    } else if uri.contains("/ddic/domains/") {
        Some("DOMA")
    } else if uri.contains("/ddic/tabletypes/") {
        Some("TTYP")
    } else if uri.contains("/messageclass/") || uri.contains("/messageclasses/") {
        Some("MSAG")
    } else {
        None
    }
}

#[inline]
const fn is_utf8_continuation_byte(byte: u8) -> bool {
    (byte & 0b1100_0000) == 0b1000_0000
}

fn utf8_safe_start(bytes: &[u8], requested_start: usize) -> usize {
    let mut start = requested_start.min(bytes.len());
    while start < bytes.len() && is_utf8_continuation_byte(bytes[start]) {
        start += 1;
    }
    start
}

fn utf8_safe_end(bytes: &[u8], requested_end: usize) -> usize {
    let mut end = requested_end.min(bytes.len());
    while end > 0 && end < bytes.len() && is_utf8_continuation_byte(bytes[end]) {
        end -= 1;
    }
    end
}

/// Which stored layer a source read actually returned, and who last changed it.
///
/// ABAP source is plain text and declares nothing about itself, and the
/// response carries no clue either — the headers for `version=active` and
/// `version=inactive` are byte-identical on an object that has only one layer.
/// The object's own document is the only thing that says, so this is a second
/// request, not a parse of the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLayer {
    /// The layer the document declared — `active`, `inactive` or `new`. SAP
    /// serves the other layer rather than refusing when the requested one does
    /// not exist, so this is not always the layer that was asked for.
    pub version: Option<String>,
    /// Who last changed the layer that arrived.
    pub changed_by: Option<String>,
}

/// Asks an object's document which layer a source read of it returned.
///
/// # Errors
///
/// Returns [`ObjectSourceError`] when the URI is not an ADT object URI, SAP
/// cannot serve the document, or it cannot be parsed.
pub async fn read_source_layer(
    sap: &SapClient,
    uri: &str,
    version: AdtVersion,
) -> Result<SourceLayer, ObjectSourceError> {
    validate_adt_object_uri(uri)?;
    // The same layer the source was asked for, so the answer describes that
    // read rather than a different one.
    let xml = sap
        .get_text_with_query(uri, &[("version", version.as_str())])
        .await?;
    let document = parse_adt_document(&xml)?;
    let root = document.root_element();
    Ok(SourceLayer {
        version: declared_version(root),
        changed_by: find_non_empty_attribute(root, "changedBy"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_paging_invariant_failure_as_an_actionable_error() {
        let encoding = ObjectSourceError::Encoding("invalid boundary".to_owned());

        assert_eq!(encoding.code(), "source_encoding_error");
        assert!(encoding.hint().is_some());
        assert_eq!(encoding.suggested_command(), None);
    }

    #[test]
    fn a_sourceless_kind_points_at_the_metadata_command() {
        let error = ObjectSourceError::NoSourceForKind {
            kind: "DOMA".to_owned(),
            uri: "/sap/bc/adt/ddic/domains/zdomain".to_owned(),
        };

        assert_eq!(error.code(), "no_source_for_kind");
        assert_eq!(
            error.suggested_command().as_deref(),
            Some("fractal object xml /sap/bc/adt/ddic/domains/zdomain")
        );
    }
}
