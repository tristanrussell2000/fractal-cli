use std::fmt::Write as _;

use serde::Serialize;

use super::{connect, tabular};
use crate::{
    cli::{VersionArgs, VersionsArgs},
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::sap::{
    adt_object_uri::validate_adt_object_uri,
    class_include::ClassInclude,
    versions::{Version, find_version, list_versions, read_version, unified_diff},
};

#[derive(Debug, Serialize)]
pub struct VersionsOutput {
    ok: bool,
    profile: String,
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<&'static str>,
    /// Numbered versions, written when a transport was released or imported.
    history: usize,
    versions: Vec<Version>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VersionOutput {
    ok: bool,
    profile: String,
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<&'static str>,
    version: Version,
    /// The version compared against, with --diff.
    #[serde(skip_serializing_if = "Option::is_none")]
    against: Option<Version>,
    /// With --diff: whether the two match, line endings aside.
    #[serde(skip_serializing_if = "Option::is_none")]
    identical: Option<bool>,
    /// The version's content, or with --diff the unified diff from the
    /// version to the one compared against.
    content: String,
}

fn object_uri(uri: &str) -> Result<String, Reported> {
    let uri = uri.trim().trim_end_matches('/');
    validate_adt_object_uri(uri)?;
    Ok(uri.trim_end_matches("/source/main").to_owned())
}

pub async fn object_versions(
    explicit_profile: Option<&str>,
    args: &VersionsArgs,
) -> Result<VersionsOutput, Reported> {
    let uri = object_uri(&args.uri)?;
    let include: Option<ClassInclude> = args.include.map(Into::into);
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let versions = list_versions(&client, &uri, include).await?;
    let history = versions
        .iter()
        .filter(|version| version.kind == "history")
        .count();
    Ok(VersionsOutput {
        ok: true,
        profile: profile_name,
        uri,
        include: include.map(ClassInclude::as_str),
        history,
        note: (history == 0).then(|| {
            "No history yet: SAP writes a version when a transport carrying the object is released or imported."
                .to_owned()
        }),
        versions,
    })
}

pub async fn object_version(
    explicit_profile: Option<&str>,
    args: &VersionArgs,
) -> Result<VersionOutput, Reported> {
    let uri = object_uri(&args.uri)?;
    let include: Option<ClassInclude> = args.include.map(Into::into);
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let versions = list_versions(&client, &uri, include).await?;
    let version = find_version(&versions, &uri, &args.version)?.clone();
    let content = read_version(&client, &version).await?;
    let (against, identical, content) = match &args.diff {
        None => (None, None, content),
        Some(other) => {
            let against = find_version(&versions, &uri, other)?.clone();
            let other_content = read_version(&client, &against).await?;
            let diff = unified_diff(&label(&version), &label(&against), &content, &other_content);
            (Some(against), Some(diff.is_empty()), diff)
        }
    };
    Ok(VersionOutput {
        ok: true,
        profile: profile_name,
        uri,
        include: include.map(ClassInclude::as_str),
        version,
        against,
        identical,
        content,
    })
}

fn label(version: &Version) -> String {
    let name = match version.kind {
        "active" => "active".to_owned(),
        "inactive" => "inactive".to_owned(),
        _ => version.id.clone(),
    };
    let details: Vec<&str> = [version.request.as_deref(), version.date.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if details.is_empty() {
        name
    } else {
        format!("{name} ({})", details.join(", "))
    }
}

pub fn print_object_versions(result: &VersionsOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    print!("{}", render_versions(result));
}

fn render_versions(result: &VersionsOutput) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "{}{}: {} version{} (newest first), {} from transports",
        result.uri,
        result
            .include
            .map_or_else(String::new, |include| format!(" ({include})")),
        result.versions.len(),
        if result.versions.len() == 1 { "" } else { "s" },
        result.history
    );
    let columns = [
        tabular::plain_column("VERSION"),
        tabular::plain_column("DATE"),
        tabular::plain_column("AUTHOR"),
        tabular::plain_column("REQUEST"),
        tabular::plain_column("DESCRIPTION"),
    ];
    let rows: Vec<_> = result
        .versions
        .iter()
        .map(|version| {
            vec![
                match version.kind {
                    "history" => version.id.clone(),
                    kind => format!("{} ({kind})", version.id),
                },
                version.date.clone().unwrap_or_default(),
                version.author.clone().unwrap_or_default(),
                version.request.clone().unwrap_or_default(),
                version.description.clone().unwrap_or_default(),
            ]
        })
        .collect();
    text.push_str(&tabular::render_grid(&columns, &rows));
    if let Some(note) = &result.note {
        let _ = writeln!(text, "note: {note}");
    }
    text
}

pub fn print_object_version(result: &VersionOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    if result.identical == Some(true) {
        let against = result.against.as_ref().map_or_else(String::new, label);
        println!("{} and {against} are identical.", label(&result.version));
        return;
    }
    print!("{}", result.content);
    if !result.content.ends_with('\n') {
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_uri_is_taken_as_its_object() {
        assert_eq!(
            object_uri("/sap/bc/adt/programs/programs/zsample/source/main").unwrap(),
            "/sap/bc/adt/programs/programs/zsample"
        );
        assert_eq!(
            object_uri("/sap/bc/adt/oo/classes/zsample/").unwrap(),
            "/sap/bc/adt/oo/classes/zsample"
        );
        assert!(object_uri("zsample").is_err());
    }
}
