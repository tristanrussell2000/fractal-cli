//! The canonical form of a metadata document.
//!
//! ADT decorates every document it serves with `atom:link` navigation
//! elements, and one of them is **conditional**: a complementary
//! active/inactive link appears on the active document as soon as somebody has
//! pending work on the object, and disappears again when they do not.
//!
//! ```xml
//! <atom:link href="./zsample?version=inactive"
//!            rel="http://www.sap.com/adt/relations/objectstates"
//!            title="Complementary active/inactive version"/>
//! ```
//!
//! So the bytes of the active document change without the object changing,
//! which would make undo's hash gate report staleness in exactly the situation
//! somebody reaches for undo. Stripping the links removes that: two reads of an
//! unchanged object then hash identically.
//!
//! The links are read-time decoration rather than part of the object, verified
//! live: a document with every one removed was accepted by `edit set-xml`, the
//! edit landed, the links were regenerated on the next read, and the object
//! activated normally. So the stripped document is what the journal stores and
//! what an undo writes back — one form, on the way in and on the way out,
//! rather than a raw hash and a canonical hash that have to be kept in step.
//!
//! `object xml` and `ddic show` still print what SAP sent. Whether they should
//! strip for readability is a question about display, not about storage.

use super::adt_response::parse_adt_document;

const ATOM_NAMESPACE: &str = "http://www.w3.org/2005/Atom";

/// Removes every `atom:link` from a metadata document.
///
/// A document that will not parse comes back unchanged: there is nothing to
/// strip, and hashing it is still deterministic.
#[must_use]
pub fn strip_navigation_links(xml: &str) -> String {
    let Ok(document) = parse_adt_document(xml) else {
        return xml.to_owned();
    };

    let links: Vec<_> = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.tag_name().name() == "link"
                && node.tag_name().namespace() == Some(ATOM_NAMESPACE)
        })
        .map(|node| node.range())
        .collect();
    if links.is_empty() {
        return xml.to_owned();
    }

    let mut stripped = String::with_capacity(xml.len());
    let mut copied = 0;
    for link in links {
        // Nested links would have overlapping ranges; ADT does not produce
        // them, and skipping one already inside a removed range keeps the
        // slicing sound if it ever does.
        if link.start < copied {
            continue;
        }
        stripped.push_str(&xml[copied..indentation_start(xml, link.start)]);
        copied = link.end;
    }
    stripped.push_str(&xml[copied..]);
    stripped
}

/// Where the whitespace directly before `start` begins.
///
/// ADT serves these documents on one line, so this usually finds nothing. It
/// matters for a pretty-printed document: without it a stripped document keeps
/// the blank line the link sat on, and a document that never had the link does
/// not, so the two would hash differently for no reason.
fn indentation_start(xml: &str, start: usize) -> usize {
    xml[..start].trim_end().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_change::source_sha256;

    /// The shape ADT actually serves: one line, no whitespace between
    /// elements, and a namespace declaration on each link.
    fn document(links: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:type="DTEL/DE" adtcore:version="active">{links}<adtcore:packageRef adtcore:name="ZSAMPLE"/><dtel:dataElement xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements"><dtel:typeKind>domain</dtel:typeKind></dtel:dataElement></blue:wbobj>"#
        )
    }

    const VERSIONS_LINK: &str = r#"<atom:link href="versions" rel="http://www.sap.com/adt/relations/versions" title="Historic versions" xmlns:atom="http://www.w3.org/2005/Atom"/>"#;
    /// The conditional one: present only while somebody has pending work.
    const STATES_LINK: &str = r#"<atom:link href="./zsample_de?version=inactive" rel="http://www.sap.com/adt/relations/objectstates" title="Complementary active/inactive version" xmlns:atom="http://www.w3.org/2005/Atom"/>"#;

    #[test]
    fn removes_every_link_and_keeps_everything_else() {
        let stripped = strip_navigation_links(&document(&format!("{VERSIONS_LINK}{STATES_LINK}")));

        assert!(!stripped.contains("atom:link"), "{stripped}");
        assert_eq!(stripped, document(""));
    }

    #[test]
    fn an_inactive_write_does_not_change_the_active_hash() {
        // The whole point. SAP adds the complementary-states link to the active
        // document once an inactive version exists, so the raw bytes differ
        // while the object does not.
        let before = document(VERSIONS_LINK);
        let after = document(&format!("{VERSIONS_LINK}{STATES_LINK}"));
        assert_ne!(source_sha256(&before), source_sha256(&after));

        assert_eq!(
            source_sha256(&strip_navigation_links(&before)),
            source_sha256(&strip_navigation_links(&after))
        );
    }

    #[test]
    fn a_document_with_no_links_is_returned_unchanged() {
        let plain = document("");
        assert_eq!(strip_navigation_links(&plain), plain);
    }

    #[test]
    fn a_pretty_printed_document_strips_to_the_same_bytes_as_one_without_links() {
        // Not the shape ADT serves, but if it ever did, leaving the blank line
        // behind would reintroduce the false staleness this exists to remove.
        let with = "<wbobj>\n  <atom:link xmlns:atom=\"http://www.w3.org/2005/Atom\" href=\"x\"/>\n  <other/>\n</wbobj>";
        let without = "<wbobj>\n  <other/>\n</wbobj>";

        assert_eq!(strip_navigation_links(with), without);
    }

    #[test]
    fn a_link_that_is_not_an_atom_link_stays() {
        // Namespace, not prefix: an element merely called `link` is the
        // object's own content and removing it would corrupt the document.
        let other = r#"<wbobj xmlns:x="urn:other"><x:link href="keep"/></wbobj>"#;
        assert_eq!(strip_navigation_links(other), other);
    }

    #[test]
    fn an_unparseable_document_comes_back_unchanged() {
        assert_eq!(strip_navigation_links("<not-closed"), "<not-closed");
    }
}
