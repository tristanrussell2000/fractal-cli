pub mod adt;
pub mod adt_message_severity;
pub mod client;
pub mod edit_session;
pub mod editable_source;
pub mod error_diagnostics;
mod inactive_source_save;
pub mod package;
pub mod source_activation;
pub mod source_check;
pub mod source_discard;
pub mod source_patch;
pub mod source_replace;
pub mod table;

/// Trims an XML attribute value and turns blank into `None`. Shared by every
/// `sap` submodule's parser — SAP consistently uses empty-but-present
/// attributes (`colType=""`, self-closed fields) to mean "no value" rather
/// than omitting the attribute.
pub(crate) fn non_empty_attribute(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Returns an XML attribute value by local name, independent of its namespace
/// prefix. SAP may bind the same ADT schema under different prefixes.
pub(crate) fn find_attribute_value<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name)
        .map(|attribute| attribute.value())
}

/// Finds the first child element with the given local (namespace-stripped)
/// tag name. Shared by every `sap` submodule's parser.
pub(crate) fn find_child<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'i>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}
