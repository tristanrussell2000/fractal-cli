use serde::Serialize;

use crate::cli::{SearchArgs, SourceArgs, UriArgs, UsagesArgs, XmlArgs};
use crate::command_error::CommandError;
use crate::commands::connect;
use crate::output::{OutputFormat, print_result};
use fractal::sap::adt::{
    ByteRangeOptions, ObjectSearchOptions, RepositoryKind, get_object_usages, search_objects,
};

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
) -> Result<ObjectSearchResultOutput, CommandError> {
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

fn parse_search_kind(value: &str) -> Result<RepositoryKind, CommandError> {
    RepositoryKind::parse(value).map_err(|error| {
        CommandError::with_hint(
            "invalid_repository_kind",
            error.to_string(),
            "Use a kind such as CLAS, INTF, TABL, PROG, DDLS, or OTHER.",
        )
    })
}

fn map_object_search_result(
    profile_name: &str,
    query: &str,
    args: &SearchArgs,
    package_patterns: Vec<String>,
    result: fractal::sap::adt::ObjectSearchResult,
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
) -> Result<ObjectSourceResultOutput, CommandError> {
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let result = fractal::sap::adt::get_source(
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
) -> Result<ObjectXmlResultOutput, CommandError> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = fractal::sap::adt::get_xml(
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
        xml: result.content,
    })
}

pub async fn object_info(
    explicit_profile: Option<&str>,
    args: &UriArgs,
) -> Result<ObjectInfoResultOutput, CommandError> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = fractal::sap::adt::get_object_info(&mut client, &args.uri).await?;

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
) -> Result<ObjectUsagesResultOutput, CommandError> {
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
    references: Vec<fractal::sap::adt::UsageReference>,
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

// `run_and_print_with` requires an operation returning `Result<T, CommandError>`;
// this handler can never fail, but must match that shape to share the runner.
#[allow(clippy::unnecessary_wraps)]
pub fn object_kinds() -> Result<ObjectKindsResultOutput, CommandError> {
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
        print_result(result, output);
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
            "/sap/bc/adt/ddic/tables/zdtls_check_in",
            "--direct-results",
        ])
        .unwrap();

        let Command::Object {
            command: ObjectCommand::Usages(args),
        } = cli.command
        else {
            panic!("expected object usages command");
        };
        assert_eq!(args.uri, "/sap/bc/adt/ddic/tables/zdtls_check_in");
        assert!(args.direct_results);
    }

    #[test]
    fn object_usages_direct_results_defaults_to_false() {
        let cli = Cli::try_parse_from([
            "fractal",
            "object",
            "usages",
            "/sap/bc/adt/ddic/tables/zdtls_check_in",
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
            fractal::sap::adt::UsageReference {
                uri: "/sap/bc/adt/ddic/structures/zdtls_check_in_s".to_owned(),
                parent_uri: Some("/sap/bc/adt/packages/zdtls".to_owned()),
                name: Some("ZDTLS_CHECK_IN_S".to_owned()),
                object_type: Some(fractal::sap::adt::AdtObjectType::parse("TABL/DS")),
                package: Some("ZDTLS".to_owned()),
                direct_result: true,
            },
            fractal::sap::adt::UsageReference {
                uri: "/sap/bc/adt/packages/zdtls".to_owned(),
                parent_uri: None,
                name: Some("ZDTLS".to_owned()),
                object_type: None,
                package: Some("ZDTLS".to_owned()),
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
            fractal::sap::adt::AdtError::InvalidUri("bad".to_owned()),
            fractal::sap::adt::AdtError::DoubledSourceSuffix("/source/main".to_owned()),
            fractal::sap::adt::AdtError::NoSourceForKind {
                kind: "DOMA".to_owned(),
                uri: "/sap/bc/adt/ddic/domains/zdomain".to_owned(),
            },
        ] {
            let command_error = CommandError::from(error);
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
        let result = fractal::sap::adt::ObjectSearchResult {
            total: 13,
            sap_search_cap: 500,
            possibly_truncated_by_sap_cap: true,
            hits: vec![fractal::sap::adt::ObjectSearchHit {
                name: "ZCL_VERSION".to_owned(),
                object_type: fractal::sap::adt::AdtObjectType::parse("CLAS/OC"),
                package: Some("ZAPP".to_owned()),
                description: Some("Version class".to_owned()),
                uri: Some("/sap/bc/adt/oo/classes/zcl_version".to_owned()),
            }],
        };
        let output = map_object_search_result(
            "DE2_903",
            "VERSION",
            &args,
            vec!["Z*".to_owned(), "Y*".to_owned()],
            result,
        );

        assert_eq!(output.profile, "DE2_903");
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
}
