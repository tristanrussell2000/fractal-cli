//! Construction of the `fractal` commands that errors offer as remedies.
//!
//! Deciding *which* remedy applies is workflow knowledge and stays with the
//! operation that failed — only that code knows a missing patch anchor means
//! "read the current source". Deciding *how the command is spelled* is the
//! CLI's knowledge, and it lives here so the subcommand paths and flag names
//! appear in exactly one place rather than being retyped beside each error.
//!
//! These builders take plain strings so this module depends on nothing: the
//! `sap` operations depend on it, never the reverse.
//!
//! Everything produced here must be read-only. A caller may execute a
//! `suggested_command` directly, so a mutation appearing in one would defeat
//! the save-only, activate-explicitly discipline the edit design rests on.
//! `src/sap/error_diagnostics.rs` asserts that rule, and `src/cli.rs` asserts
//! that every command built here actually parses as a CLI invocation.

/// Verifies connectivity, credentials, and the selected client.
///
/// Takes no arguments, so transport failures can name it without knowing which
/// profile was selected.
#[must_use]
pub fn system_test() -> String {
    "fractal system test".to_owned()
}

/// Reads one stored source version of an editable object.
#[must_use]
pub fn edit_read(object_type: &str, name: &str, version: &str) -> String {
    edit("read", object_type, name, version)
}

/// Syntax-checks one stored source version of an editable object.
#[must_use]
pub fn edit_check(object_type: &str, name: &str, version: &str) -> String {
    edit("check", object_type, name, version)
}

/// Locates an object whose name may be wrong.
#[must_use]
pub fn object_search(object_type: &str, name: &str) -> String {
    format!("fractal object search {name} --kind {object_type}")
}

/// Lists the caller's own modifiable change requests.
#[must_use]
pub fn transport_list() -> String {
    "fractal transport list".to_owned()
}

/// Lists every repository kind the CLI accepts.
#[must_use]
pub fn object_kinds() -> String {
    "fractal object kinds".to_owned()
}

/// Retrieves an object's full metadata XML.
#[must_use]
pub fn object_xml(uri: &str) -> String {
    format!("fractal object xml {uri}")
}

/// Lists the fields of one DDIC entity.
#[must_use]
pub fn table_metadata(entity: &str) -> String {
    format!("fractal table metadata {entity}")
}

fn edit(operation: &str, object_type: &str, name: &str, version: &str) -> String {
    format!("fractal edit {operation} --type {object_type} --name {name} --version {version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_documented_command_spellings() {
        assert_eq!(system_test(), "fractal system test");
        assert_eq!(
            edit_read("CLAS", "ZCL_EXAMPLE", "inactive"),
            "fractal edit read --type CLAS --name ZCL_EXAMPLE --version inactive"
        );
        assert_eq!(
            edit_check("DDLS", "ZVIEW", "active"),
            "fractal edit check --type DDLS --name ZVIEW --version active"
        );
        assert_eq!(
            object_search("INTF", "ZIF_MISSING"),
            "fractal object search ZIF_MISSING --kind INTF"
        );
        assert_eq!(object_kinds(), "fractal object kinds");
        assert_eq!(
            object_xml("/sap/bc/adt/ddic/domains/zdomain"),
            "fractal object xml /sap/bc/adt/ddic/domains/zdomain"
        );
        assert_eq!(
            table_metadata("ZDEMO_EVENT_LOG"),
            "fractal table metadata ZDEMO_EVENT_LOG"
        );
    }

    #[test]
    fn preserves_registered_namespace_names() {
        assert_eq!(
            edit_read("CLAS", "/ACME/EXAMPLE", "active"),
            "fractal edit read --type CLAS --name /ACME/EXAMPLE --version active"
        );
    }
}
