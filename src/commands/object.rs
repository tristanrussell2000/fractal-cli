use std::fmt::Write as _;

use serde::Serialize;

use crate::cli::{SearchArgs, SourceArgs, UriArgs, UsagesArgs, XmlArgs};
use crate::commands::{connect, tabular};
use crate::output::{OutputFormat, print_json};
use crate::reported::Reported;
use fractal::sap::{
    object_search::{ObjectSearchOptions, search_objects},
    object_source::ByteRangeOptions,
    object_usages::get_object_usages,
    repository_kind::RepositoryKind,
};
use fractal::source_change::source_sha256;

#[derive(Debug, Serialize)]
pub struct ObjectSearchResultOutput {
    ok: bool,
    profile: String,
    query: String,
    package_patterns: Vec<String>,
    package_patterns_source: String,
    total_matching: usize,
    returned: usize,
    offset: usize,
    limit: usize,
    next_offset: Option<usize>,
    sap_search_cap: usize,
    possibly_truncated_by_sap_cap: bool,
    hits: Vec<ObjectSearchHitOutput>,
}

#[derive(Debug, Serialize)]
struct ObjectSearchHitOutput {
    name: String,
    kind: String,
    object_type: String,
    package: Option<String>,
    description: Option<String>,
    uri: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ObjectSourceResultOutput {
    ok: bool,
    profile: String,
    uri: String,
    start_byte: usize,
    end_byte: usize,
    total_bytes: usize,
    truncated: bool,
    next_offset: Option<usize>,
    source: String,
}

#[derive(Debug, Serialize)]
pub struct ObjectXmlResultOutput {
    ok: bool,
    profile: String,
    uri: String,
    /// SHA-256 of the document as read, so a later `edit set-xml` can pass it
    /// as `--expected-sha256` and refuse to overwrite a changed document.
    sha256: String,
    xml: String,
}

#[derive(Debug, Serialize)]
pub struct ObjectInfoResultOutput {
    ok: bool,
    profile: String,
    uri: String,
    description: String,
}

#[derive(Debug, Serialize)]
pub struct ObjectUsagesResultOutput {
    ok: bool,
    profile: String,
    uri: String,
    direct_results_only: bool,
    total: usize,
    direct_results: usize,
    references: Vec<UsageReferenceOutput>,
}

#[derive(Debug, Serialize)]
struct UsageReferenceOutput {
    uri: String,
    parent_uri: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    object_type: Option<String>,
    package: Option<String>,
    direct_result: bool,
}

#[derive(Debug, Serialize)]
pub struct ObjectKindsResultOutput {
    ok: bool,
    kinds: Vec<ObjectKindOutput>,
}

#[derive(Debug, Serialize)]
struct ObjectKindOutput {
    kind: String,
    description: String,
}

pub async fn object_search(
    explicit_profile: Option<&str>,
    args: &SearchArgs,
) -> Result<ObjectSearchResultOutput, Reported> {
    let kind = args.kind.as_deref().map(parse_search_kind).transpose()?;
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let explicit_patterns =
        (!args.package_patterns.is_empty()).then(|| args.package_patterns.clone());
    let effective_patterns = explicit_patterns
        .clone()
        .unwrap_or_else(|| profile.customer_namespaces.clone());
    let result = search_objects(
        &mut client,
        &profile,
        &args.query,
        ObjectSearchOptions {
            package_patterns: explicit_patterns,
            kind,
            offset: args.offset,
            limit: Some(args.limit),
        },
    )
    .await?;
    Ok(map_object_search_result(
        &profile_name,
        &args.query,
        args,
        effective_patterns,
        result,
    ))
}

fn parse_search_kind(value: &str) -> Result<RepositoryKind, Reported> {
    Ok(RepositoryKind::parse(value)?)
}

fn map_object_search_result(
    profile_name: &str,
    query: &str,
    args: &SearchArgs,
    package_patterns: Vec<String>,
    result: fractal::sap::object_search::ObjectSearchResult,
) -> ObjectSearchResultOutput {
    let returned = result.hits.len();
    let next_offset = (args.offset + returned < result.total).then_some(args.offset + returned);

    ObjectSearchResultOutput {
        ok: true,
        profile: profile_name.to_owned(),
        query: query.to_owned(),
        package_patterns,
        package_patterns_source: if args.package_patterns.is_empty() {
            "default".to_owned()
        } else {
            "explicit".to_owned()
        },
        total_matching: result.total,
        returned,
        offset: args.offset,
        limit: args.limit,
        next_offset,
        sap_search_cap: result.sap_search_cap,
        possibly_truncated_by_sap_cap: result.possibly_truncated_by_sap_cap,
        hits: result
            .hits
            .into_iter()
            .map(|hit| ObjectSearchHitOutput {
                name: hit.name,
                kind: hit.object_type.kind().as_str().to_owned(),
                object_type: hit.object_type.as_str().to_owned(),
                package: hit.package,
                description: hit.description,
                uri: hit.uri,
            })
            .collect(),
    }
}

pub async fn object_source(
    explicit_profile: Option<&str>,
    args: &SourceArgs,
) -> Result<ObjectSourceResultOutput, Reported> {
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let result = fractal::sap::object_source::get_source(
        &client,
        &args.uri,
        ByteRangeOptions {
            offset: args.offset,
            limit: args.limit,
        },
    )
    .await?;

    Ok(ObjectSourceResultOutput {
        ok: true,
        profile: profile_name,
        uri: args.uri.clone(),
        start_byte: result.start_byte,
        end_byte: result.end_byte,
        total_bytes: result.total_bytes,
        truncated: result.truncated,
        next_offset: result.next_offset,
        source: result.content,
    })
}

pub async fn object_xml(
    explicit_profile: Option<&str>,
    args: &XmlArgs,
) -> Result<ObjectXmlResultOutput, Reported> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = fractal::sap::object_source::get_xml(
        &mut client,
        &args.uri,
        ByteRangeOptions {
            offset: args.offset,
            limit: args.limit,
        },
    )
    .await?;

    Ok(ObjectXmlResultOutput {
        ok: true,
        profile: profile_name,
        uri: args.uri.clone(),
        sha256: source_sha256(&result.content),
        xml: result.content,
    })
}

pub async fn object_info(
    explicit_profile: Option<&str>,
    args: &UriArgs,
) -> Result<ObjectInfoResultOutput, Reported> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = fractal::sap::object_info::get_object_info(&mut client, &args.uri).await?;

    Ok(ObjectInfoResultOutput {
        ok: true,
        profile: profile_name,
        uri: result.uri,
        description: result.description,
    })
}

pub async fn object_usages(
    explicit_profile: Option<&str>,
    args: &UsagesArgs,
) -> Result<ObjectUsagesResultOutput, Reported> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let references = get_object_usages(&mut client, &args.uri).await?;
    let total = references.len();
    let direct_results = references
        .iter()
        .filter(|reference| reference.direct_result)
        .count();
    let filtered = if args.direct_results {
        references
            .into_iter()
            .filter(|reference| reference.direct_result)
            .collect()
    } else {
        references
    };

    Ok(ObjectUsagesResultOutput {
        ok: true,
        profile: profile_name,
        uri: args.uri.clone(),
        direct_results_only: args.direct_results,
        total,
        direct_results,
        references: map_usage_references(filtered),
    })
}

fn map_usage_references(
    references: Vec<fractal::sap::object_usages::UsageReference>,
) -> Vec<UsageReferenceOutput> {
    references
        .into_iter()
        .map(|reference| UsageReferenceOutput {
            uri: reference.uri,
            parent_uri: reference.parent_uri,
            name: reference.name,
            kind: reference
                .object_type
                .as_ref()
                .map(|object_type| object_type.kind().as_str().to_owned()),
            object_type: reference
                .object_type
                .map(|object_type| object_type.as_str().to_owned()),
            package: reference.package,
            direct_result: reference.direct_result,
        })
        .collect()
}

// `run_and_print_with` requires an operation returning `Result<T, Reported>`;
// this handler can never fail, but must match that shape to share the runner.
#[allow(clippy::unnecessary_wraps)]
pub fn object_kinds() -> Result<ObjectKindsResultOutput, Reported> {
    Ok(ObjectKindsResultOutput {
        ok: true,
        kinds: RepositoryKind::ALL
            .into_iter()
            .map(|kind| ObjectKindOutput {
                kind: kind.as_str().to_owned(),
                description: kind.description().to_owned(),
            })
            .collect(),
    })
}

pub fn print_object_kinds(result: &ObjectKindsResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    let width = result
        .kinds
        .iter()
        .map(|kind| kind.kind.len())
        .max()
        .unwrap_or(0);
    for kind in &result.kinds {
        println!("{:width$}  {}", kind.kind, kind.description);
    }
}

pub fn print_object_search(result: &ObjectSearchResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    print!("{}", render_object_search_readable(result));
}

fn render_object_search_readable(result: &ObjectSearchResultOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(output, "query: {}", result.query);
    let _ = writeln!(
        output,
        "packages: {} ({})",
        result.package_patterns.join(", "),
        result.package_patterns_source
    );
    let _ = writeln!(
        output,
        "hits: {} of {} (offset {}, limit {})",
        result.returned, result.total_matching, result.offset, result.limit
    );
    if let Some(next_offset) = result.next_offset {
        let _ = writeln!(output, "next offset: {next_offset}");
    }
    if result.possibly_truncated_by_sap_cap {
        let _ = writeln!(
            output,
            "warning: SAP caps a search at {} hits, so matches may be missing",
            result.sap_search_cap
        );
    }

    let columns = [
        tabular::plain_column("TYPE"),
        tabular::plain_column("NAME"),
        tabular::plain_column("PACKAGE"),
        tabular::plain_column("DESCRIPTION"),
    ];
    let rows: Vec<Vec<String>> = result
        .hits
        .iter()
        .map(|hit| {
            vec![
                hit.object_type.clone(),
                hit.name.clone(),
                optional_cell(hit.package.as_deref()),
                optional_cell(hit.description.as_deref()),
            ]
        })
        .collect();
    output.push_str(&tabular::render_grid(&columns, &rows));
    output
}

pub fn print_object_usages(result: &ObjectUsagesResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    print!("{}", render_object_usages_readable(result));
}

fn render_object_usages_readable(result: &ObjectUsagesResultOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(output, "uri: {}", result.uri);
    let _ = writeln!(
        output,
        "references: {} (direct: {}){}",
        result.total,
        result.direct_results,
        if result.direct_results_only {
            ", showing direct only"
        } else {
            ""
        }
    );

    // A URI is meant to be pasted into the next command, so it gets a line of
    // its own rather than a grid cell that would truncate it.
    for reference in &result.references {
        // Once the filter is on every reference is direct, so saying so adds
        // nothing.
        let direct = if reference.direct_result && !result.direct_results_only {
            "  (direct)"
        } else {
            ""
        };
        let _ = writeln!(
            output,
            "- {} {} [{}]{direct}",
            optional_cell(reference.object_type.as_deref()),
            optional_cell(reference.name.as_deref()),
            optional_cell(reference.package.as_deref())
        );
        let _ = writeln!(output, "  {}", reference.uri);
    }
    output
}

fn optional_cell(value: Option<&str>) -> String {
    value.unwrap_or("-").to_owned()
}

pub fn print_object_info(result: &ObjectInfoResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    println!("profile: {}", result.profile);
    println!("uri: {}", result.uri);
    println!("description: {}", result.description);
}

pub fn print_object_source(result: &ObjectSourceResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    print!("{}", render_object_source_readable(result));
}

fn render_object_source_readable(result: &ObjectSourceResultOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(output, "uri: {}", result.uri);
    let _ = writeln!(
        output,
        "bytes: {}-{} of {}",
        result.start_byte, result.end_byte, result.total_bytes
    );
    if result.truncated {
        let _ = writeln!(
            output,
            "truncated: yes (next offset: {})",
            result
                .next_offset
                .map_or_else(|| "-".to_owned(), |offset| offset.to_string())
        );
    }
    output.push_str("\nsource:\n");
    output.push_str(&result.source);
    output
}

pub fn print_object_xml(result: &ObjectXmlResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    print!("{}", render_object_xml_readable(result));
}

fn render_object_xml_readable(result: &ObjectXmlResultOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(output, "uri: {}", result.uri);
    let _ = writeln!(output, "sha256: {}", result.sha256);
    output.push_str("\nxml:\n");
    output.push_str(&result.xml);
    output
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, ObjectCommand};

    #[test]
    fn parses_object_search_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "object",
            "search",
            "VERSION",
            "--kind",
            "clas",
            "--package-pattern",
            "ZAPP*",
            "--package-pattern",
            "YLIB*",
            "--offset",
            "2",
            "--limit",
            "5",
        ])
        .unwrap();

        let Command::Object {
            command: ObjectCommand::Search(args),
        } = cli.command
        else {
            panic!("expected object search command");
        };
        assert_eq!(args.query, "VERSION");
        assert_eq!(args.kind.as_deref(), Some("clas"));
        assert_eq!(args.package_patterns, vec!["ZAPP*", "YLIB*"]);
        assert_eq!(args.offset, 2);
        assert_eq!(args.limit, 5);
    }

    #[test]
    fn parses_search_kind_case_insensitively() {
        assert_eq!(parse_search_kind("cLaS").unwrap(), RepositoryKind::Clas);
    }

    #[test]
    fn invalid_search_kind_has_a_cli_hint() {
        let error = parse_search_kind("NOPE").unwrap_err();
        assert_eq!(error.code(), "invalid_repository_kind");
        assert!(error.hint().unwrap().contains("CLAS"));
    }

    #[test]
    fn parses_object_xml_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "object",
            "xml",
            "/sap/bc/adt/oo/classes/zcl_test",
            "--offset",
            "100",
            "--limit",
            "500",
        ])
        .unwrap();

        let Command::Object {
            command: ObjectCommand::Xml(args),
        } = cli.command
        else {
            panic!("expected object xml command");
        };
        assert_eq!(args.uri, "/sap/bc/adt/oo/classes/zcl_test");
        assert_eq!(args.offset, 100);
        assert_eq!(args.limit, Some(500));
    }

    #[test]
    fn parses_object_source_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "object",
            "source",
            "/sap/bc/adt/oo/classes/zcl_test",
            "--offset",
            "100",
            "--limit",
            "500",
        ])
        .unwrap();

        let Command::Object {
            command: ObjectCommand::Source(args),
        } = cli.command
        else {
            panic!("expected object source command");
        };
        assert_eq!(args.uri, "/sap/bc/adt/oo/classes/zcl_test");
        assert_eq!(args.offset, 100);
        assert_eq!(args.limit, Some(500));
    }

    #[test]
    fn parses_object_info_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "object",
            "info",
            "/sap/bc/adt/oo/classes/zcl_test",
        ])
        .unwrap();

        let Command::Object {
            command: ObjectCommand::Info(args),
        } = cli.command
        else {
            panic!("expected object info command");
        };
        assert_eq!(args.uri, "/sap/bc/adt/oo/classes/zcl_test");
    }

    #[test]
    fn parses_object_usages_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "object",
            "usages",
            "/sap/bc/adt/ddic/tables/zexample_table",
            "--direct-results",
        ])
        .unwrap();

        let Command::Object {
            command: ObjectCommand::Usages(args),
        } = cli.command
        else {
            panic!("expected object usages command");
        };
        assert_eq!(args.uri, "/sap/bc/adt/ddic/tables/zexample_table");
        assert!(args.direct_results);
    }

    #[test]
    fn object_usages_direct_results_defaults_to_false() {
        let cli = Cli::try_parse_from([
            "fractal",
            "object",
            "usages",
            "/sap/bc/adt/ddic/tables/zexample_table",
        ])
        .unwrap();

        let Command::Object {
            command: ObjectCommand::Usages(args),
        } = cli.command
        else {
            panic!("expected object usages command");
        };
        assert!(!args.direct_results);
    }

    #[test]
    fn maps_usage_references_and_computes_direct_result_kind_and_type() {
        let refs = vec![
            fractal::sap::object_usages::UsageReference {
                uri: "/sap/bc/adt/ddic/structures/zexample_table_s".to_owned(),
                parent_uri: Some("/sap/bc/adt/packages/zexample".to_owned()),
                name: Some("ZEXAMPLE_TABLE_S".to_owned()),
                object_type: Some(fractal::sap::repository_kind::AdtObjectType::parse(
                    "TABL/DS",
                )),
                package: Some("ZEXAMPLE".to_owned()),
                direct_result: true,
            },
            fractal::sap::object_usages::UsageReference {
                uri: "/sap/bc/adt/packages/zexample".to_owned(),
                parent_uri: None,
                name: Some("ZEXAMPLE".to_owned()),
                object_type: None,
                package: Some("ZEXAMPLE".to_owned()),
                direct_result: false,
            },
        ];

        let mapped = map_usage_references(refs);
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].kind.as_deref(), Some("STRU"));
        assert_eq!(mapped[0].object_type.as_deref(), Some("TABL/DS"));
        assert!(mapped[0].direct_result);
        assert_eq!(mapped[1].kind, None);
        assert_eq!(mapped[1].object_type, None);
        assert!(!mapped[1].direct_result);
    }

    #[test]
    fn parses_object_kinds_command_from_cli() {
        let cli = Cli::try_parse_from(["fractal", "object", "kinds"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Object {
                command: ObjectCommand::Kinds
            }
        ));
    }

    #[test]
    fn every_repository_kind_has_a_stable_code_and_a_description() {
        let result = object_kinds().unwrap();
        assert_eq!(result.kinds.len(), RepositoryKind::ALL.len());
        for kind in &result.kinds {
            assert!(!kind.kind.is_empty());
            assert!(!kind.description.is_empty());
        }
    }

    #[test]
    fn source_adt_errors_have_structured_cli_codes() {
        for error in [
            fractal::sap::object_source::ObjectSourceError::Uri(
                fractal::sap::adt_object_uri::AdtObjectUriError::NotAnAdtUri("bad".to_owned()),
            ),
            fractal::sap::object_source::ObjectSourceError::Uri(
                fractal::sap::adt_object_uri::AdtObjectUriError::DoubledSourceSuffix(
                    "/source/main".to_owned(),
                ),
            ),
            fractal::sap::object_source::ObjectSourceError::NoSourceForKind {
                kind: "DOMA".to_owned(),
                uri: "/sap/bc/adt/ddic/domains/zdomain".to_owned(),
            },
        ] {
            let command_error = Reported::from(error);
            assert!(!command_error.code().is_empty());
            assert!(command_error.hint().is_some());
        }
    }

    #[test]
    fn maps_search_results_and_preserves_pagination_and_cap_warning() {
        let args = SearchArgs {
            query: "VERSION".to_owned(),
            kind: Some("CLAS".to_owned()),
            package_patterns: vec![],
            offset: 10,
            limit: 2,
        };
        let result = fractal::sap::object_search::ObjectSearchResult {
            total: 13,
            sap_search_cap: 500,
            possibly_truncated_by_sap_cap: true,
            hits: vec![fractal::sap::object_search::ObjectSearchHit {
                name: "ZCL_VERSION".to_owned(),
                object_type: fractal::sap::repository_kind::AdtObjectType::parse("CLAS/OC"),
                package: Some("ZAPP".to_owned()),
                description: Some("Version class".to_owned()),
                uri: Some("/sap/bc/adt/oo/classes/zcl_version".to_owned()),
            }],
        };
        let output = map_object_search_result(
            "DEV_100",
            "VERSION",
            &args,
            vec!["Z*".to_owned(), "Y*".to_owned()],
            result,
        );

        assert_eq!(output.profile, "DEV_100");
        assert_eq!(output.package_patterns_source, "default");
        assert_eq!(output.total_matching, 13);
        assert_eq!(output.returned, 1);
        assert_eq!(output.offset, 10);
        assert_eq!(output.limit, 2);
        assert_eq!(output.next_offset, Some(11));
        assert!(output.possibly_truncated_by_sap_cap);
        assert_eq!(output.hits[0].kind, "CLAS");
        assert_eq!(output.hits[0].name, "ZCL_VERSION");
    }

    #[test]
    fn readable_search_output_tabulates_the_hits() {
        let result = ObjectSearchResultOutput {
            ok: true,
            profile: "de2".to_owned(),
            query: "SAMPLE".to_owned(),
            package_patterns: vec!["ZAPP*".to_owned()],
            package_patterns_source: "default".to_owned(),
            total_matching: 42,
            returned: 1,
            offset: 0,
            limit: 1,
            next_offset: Some(1),
            sap_search_cap: 100,
            possibly_truncated_by_sap_cap: true,
            hits: vec![ObjectSearchHitOutput {
                name: "ZCL_SAMPLE".to_owned(),
                kind: "CLAS".to_owned(),
                object_type: "CLAS/OC".to_owned(),
                package: None,
                description: Some("Sample class".to_owned()),
                uri: Some("/sap/bc/adt/oo/classes/zcl_sample".to_owned()),
            }],
        };

        let rendered = render_object_search_readable(&result);

        assert!(rendered.contains("hits: 1 of 42 (offset 0, limit 1)"));
        assert!(rendered.contains("next offset: 1"));
        assert!(rendered.contains("SAP caps a search at 100 hits"));
        assert!(rendered.contains("ZCL_SAMPLE"));
        assert!(rendered.contains("Sample class"));
    }

    #[test]
    fn readable_usages_output_keeps_every_uri_whole() {
        let long_uri =
            "/sap/bc/adt/ddic/tables/zsample_long_table_name_that_runs_past_a_grid_cell".to_owned();
        let reference = UsageReferenceOutput {
            uri: long_uri.clone(),
            parent_uri: None,
            name: Some("ZCL_CALLER".to_owned()),
            kind: Some("CLAS".to_owned()),
            object_type: Some("CLAS/OC".to_owned()),
            package: Some("ZAPP".to_owned()),
            direct_result: true,
        };
        let mut result = ObjectUsagesResultOutput {
            ok: true,
            profile: "de2".to_owned(),
            uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
            direct_results_only: false,
            total: 1,
            direct_results: 1,
            references: vec![reference],
        };

        let all = render_object_usages_readable(&result);
        assert!(all.contains(&long_uri));
        assert!(!all.contains('…'));
        assert!(all.contains("(direct)"));
        assert!(all.contains("references: 1 (direct: 1)"));

        result.direct_results_only = true;
        let direct_only = render_object_usages_readable(&result);
        assert!(!direct_only.contains("(direct)"));
        assert!(direct_only.contains("showing direct only"));
        assert!(direct_only.contains("ZCL_CALLER"));
    }

    #[test]
    fn readable_source_output_keeps_the_source_verbatim() {
        let result = ObjectSourceResultOutput {
            ok: true,
            profile: "de2".to_owned(),
            uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
            start_byte: 0,
            end_byte: 24,
            total_bytes: 96,
            truncated: true,
            next_offset: Some(24),
            source: "CLASS zcl_sample DEFINITION.".to_owned(),
        };

        let rendered = render_object_source_readable(&result);

        assert!(rendered.contains("bytes: 0-24 of 96"));
        assert!(rendered.contains("truncated: yes (next offset: 24)"));
        assert!(rendered.ends_with("CLASS zcl_sample DEFINITION."));
        assert!(!rendered.contains('{'));
    }

    #[test]
    fn readable_source_output_omits_truncation_when_the_whole_source_is_returned() {
        let result = ObjectSourceResultOutput {
            ok: true,
            profile: "de2".to_owned(),
            uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
            start_byte: 0,
            end_byte: 6,
            total_bytes: 6,
            truncated: false,
            next_offset: None,
            source: "REPORT".to_owned(),
        };

        assert!(!render_object_source_readable(&result).contains("truncated"));
    }

    #[test]
    fn readable_xml_output_keeps_the_document_verbatim() {
        let result = ObjectXmlResultOutput {
            ok: true,
            profile: "de2".to_owned(),
            uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
            sha256: "abc123".to_owned(),
            xml: "<class:abapClass/>".to_owned(),
        };

        let rendered = render_object_xml_readable(&result);

        assert!(rendered.contains("sha256: abc123"));
        assert!(rendered.ends_with("<class:abapClass/>"));
    }
}
