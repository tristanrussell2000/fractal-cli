//! Every integration suite, compiled into one test binary.
//!
//! Each file under `main/` used to be its own `tests/*.rs`, and cargo built and
//! ran 31 separate executables. The tests themselves take well under a second
//! in total; the cost was that macOS stalls the *first* execution of each
//! freshly linked binary for over a minute, so a full run paid that toll 31
//! times. One binary pays it once.
//!
//! The `#[path]` attributes are needed because this file is a crate root, so a
//! bare `mod x;` would look for `tests/x.rs` — which is exactly the per-file
//! test target being avoided.

/// The shared ADT edit-session mock, declared once rather than compiled into
/// each suite the way separate test binaries forced.
#[path = "main/adt_edit_mock/mod.rs"]
mod adt_edit_mock;

#[path = "main/adt.rs"]
mod adt;
#[path = "main/class_run.rs"]
mod class_run;
#[path = "main/ddic_type.rs"]
mod ddic_type;
#[path = "main/edit_activation.rs"]
mod edit_activation;
#[path = "main/edit_check.rs"]
mod edit_check;
#[path = "main/edit_create.rs"]
mod edit_create;
#[path = "main/edit_delete.rs"]
mod edit_delete;
#[path = "main/edit_discard.rs"]
mod edit_discard;
#[path = "main/edit_package_allowlist.rs"]
mod edit_package_allowlist;
#[path = "main/edit_patch.rs"]
mod edit_patch;
#[path = "main/edit_replace.rs"]
mod edit_replace;
#[path = "main/edit_source.rs"]
mod edit_source;
#[path = "main/error_reporting.rs"]
mod error_reporting;
#[path = "main/metadata_activation.rs"]
mod metadata_activation;
#[path = "main/metadata_create.rs"]
mod metadata_create;
#[path = "main/metadata_write.rs"]
mod metadata_write;
#[path = "main/object_info.rs"]
mod object_info;
#[path = "main/object_usages.rs"]
mod object_usages;
#[path = "main/package.rs"]
mod package;
#[path = "main/package_items.rs"]
mod package_items;
#[path = "main/package_tree.rs"]
mod package_tree;
#[path = "main/query.rs"]
mod query;
#[path = "main/sap_client.rs"]
mod sap_client;
#[path = "main/source.rs"]
mod source;
#[path = "main/table_data.rs"]
mod table_data;
#[path = "main/table_metadata.rs"]
mod table_metadata;
#[path = "main/transport_create.rs"]
mod transport_create;
#[path = "main/transport_list.rs"]
mod transport_list;
#[path = "main/transport_show.rs"]
mod transport_show;
#[path = "main/undo.rs"]
mod undo;
#[path = "main/xml.rs"]
mod xml;
