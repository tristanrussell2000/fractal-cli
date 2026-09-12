use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Serialize;

use crate::cli::PackageItemsArgs;
use crate::commands::{connect, tabular};
use crate::output::{OutputFormat, print_result};
use crate::{cli::PackageTreeArgs, reported::Reported};
use fractal::sap::{
    package::{PackageItemsOptions, get_package_items, get_package_tree},
    repository_kind::RepositoryKind,
};

#[derive(Debug, Serialize)]
pub struct PackageTreeResultOutput {
    ok: bool,
    profile: String,
    root: String,
    recursive: bool,
    packages_walked: usize,
    packages: Vec<PackageTreeNodeOutput>,
    kinds: BTreeMap<String, usize>,
    packages_failed: Vec<PackageFailureOutput>,
}

#[derive(Debug, Serialize)]
struct PackageTreeNodeOutput {
    name: String,
    parent: Option<String>,
    description: Option<String>,
    item_count: usize,
}

#[derive(Debug, Serialize)]
struct PackageFailureOutput {
    package: String,
    message: String,
}

#[derive(Debug, Serialize)]
pub struct PackageItemsResultOutput {
    ok: bool,
    profile: String,
    root: String,
    recursive: bool,
    total_matching: usize,
    returned: usize,
    offset: usize,
    limit: usize,
    next_offset: Option<usize>,
    kinds: BTreeMap<String, usize>,
    packages_failed: Vec<PackageFailureOutput>,
    items: Vec<PackageItemOutput>,
}

#[derive(Debug, Serialize)]
struct PackageItemOutput {
    name: String,
    kind: String,
    object_type: String,
    package: String,
    description: Option<String>,
    uri: Option<String>,
}

pub async fn package_tree(
    explicit_profile: Option<&str>,
    args: &PackageTreeArgs,
) -> Result<PackageTreeResultOutput, Reported> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let recursive = !args.no_recursive;
    let tree = get_package_tree(&mut client, &args.name, recursive).await?;

    Ok(PackageTreeResultOutput {
        ok: true,
        profile: profile_name,
        root: tree.root,
        recursive,
        packages_walked: tree.packages.len(),
        packages: tree
            .packages
            .into_iter()
            .map(|package| PackageTreeNodeOutput {
                name: package.name,
                parent: package.parent,
                description: package.description,
                item_count: package.item_count,
            })
            .collect(),
        kinds: tree.kinds,
        packages_failed: tree
            .packages_failed
            .into_iter()
            .map(|failure| PackageFailureOutput {
                package: failure.package,
                message: failure.message,
            })
            .collect(),
    })
}

pub async fn package_items(
    explicit_profile: Option<&str>,
    args: &PackageItemsArgs,
) -> Result<PackageItemsResultOutput, Reported> {
    let kind = args
        .kind
        .as_deref()
        .map(RepositoryKind::parse)
        .transpose()?;
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = get_package_items(
        &mut client,
        &args.name,
        PackageItemsOptions {
            recursive: args.recursive,
            kind,
            object_type: args.object_type.clone(),
            name_substring: args.name_substring.clone(),
            offset: args.offset,
            limit: args.limit,
        },
    )
    .await?;
    let returned = result.items.len();
    let next_offset = (args.offset + returned < result.total).then_some(args.offset + returned);

    Ok(PackageItemsResultOutput {
        ok: true,
        profile: profile_name,
        root: result.root,
        recursive: result.recursive,
        total_matching: result.total,
        returned,
        offset: args.offset,
        limit: args.limit,
        next_offset,
        kinds: result.kinds,
        packages_failed: result
            .packages_failed
            .into_iter()
            .map(|failure| PackageFailureOutput {
                package: failure.package,
                message: failure.message,
            })
            .collect(),
        items: result
            .items
            .into_iter()
            .map(|item| PackageItemOutput {
                name: item.name,
                kind: item.object_type.kind().as_str().to_owned(),
                object_type: item.object_type.as_str().to_owned(),
                package: item.package,
                description: item.description,
                uri: item.uri,
            })
            .collect(),
    })
}

pub fn print_package_tree(result: &PackageTreeResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_package_tree_readable(result));
}

fn render_package_tree_readable(result: &PackageTreeResultOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(
        output,
        "root: {}{}",
        result.root,
        if result.recursive {
            ""
        } else {
            " (this package only)"
        }
    );
    let _ = writeln!(output, "packages walked: {}", result.packages_walked);
    if !result.kinds.is_empty() {
        let _ = writeln!(output, "kinds: {}", render_kind_counts(&result.kinds));
    }
    output.push('\n');
    write_package_children(&mut output, result, None, 0, &mut BTreeSet::new());
    write_package_failures(&mut output, &result.packages_failed);
    output
}

/// Children are found by parent name, so a package whose parent was never
/// walked would otherwise be dropped: those are written at the root, after the
/// packages that do hang off it.
fn write_package_children(
    output: &mut String,
    result: &PackageTreeResultOutput,
    parent: Option<&str>,
    depth: usize,
    written: &mut BTreeSet<String>,
) {
    let walked: BTreeSet<&str> = result
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    for package in &result.packages {
        let hangs_here = match parent {
            Some(parent) => package.parent.as_deref() == Some(parent),
            None => package
                .parent
                .as_deref()
                .is_none_or(|parent| !walked.contains(parent)),
        };
        if !hangs_here || !written.insert(package.name.clone()) {
            continue;
        }

        let _ = writeln!(
            output,
            "{}{}  ({} item(s)){}",
            "  ".repeat(depth),
            package.name,
            package.item_count,
            package
                .description
                .as_deref()
                .map_or_else(String::new, |description| format!("  {description}"))
        );
        write_package_children(output, result, Some(&package.name), depth + 1, written);
    }
}

pub fn print_package_items(result: &PackageItemsResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_package_items_readable(result));
}

fn render_package_items_readable(result: &PackageItemsResultOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(
        output,
        "root: {}{}",
        result.root,
        if result.recursive {
            ""
        } else {
            " (this package only)"
        }
    );
    let _ = writeln!(
        output,
        "items: {} of {} (offset {}, limit {})",
        result.returned, result.total_matching, result.offset, result.limit
    );
    if let Some(next_offset) = result.next_offset {
        let _ = writeln!(output, "next offset: {next_offset}");
    }
    if !result.kinds.is_empty() {
        let _ = writeln!(output, "kinds: {}", render_kind_counts(&result.kinds));
    }

    let columns = [
        tabular::plain_column("TYPE"),
        tabular::plain_column("NAME"),
        tabular::plain_column("PACKAGE"),
        tabular::plain_column("DESCRIPTION"),
    ];
    let rows: Vec<Vec<String>> = result
        .items
        .iter()
        .map(|item| {
            vec![
                item.object_type.clone(),
                item.name.clone(),
                item.package.clone(),
                item.description.clone().unwrap_or_else(|| "-".to_owned()),
            ]
        })
        .collect();
    output.push_str(&tabular::render_grid(&columns, &rows));
    write_package_failures(&mut output, &result.packages_failed);
    output
}

fn render_kind_counts(kinds: &BTreeMap<String, usize>) -> String {
    kinds
        .iter()
        .map(|(kind, count)| format!("{kind} {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_package_failures(output: &mut String, failures: &[PackageFailureOutput]) {
    if failures.is_empty() {
        return;
    }

    let _ = writeln!(output, "\n{} package(s) could not be read:", failures.len());
    for failure in failures {
        let _ = writeln!(output, "  ! {} - {}", failure.package, failure.message);
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, PackageCommand};

    fn node(name: &str, parent: Option<&str>, item_count: usize) -> PackageTreeNodeOutput {
        PackageTreeNodeOutput {
            name: name.to_owned(),
            parent: parent.map(ToOwned::to_owned),
            description: None,
            item_count,
        }
    }

    #[test]
    fn readable_tree_output_nests_a_package_under_its_parent() {
        let result = PackageTreeResultOutput {
            ok: true,
            profile: "de2".to_owned(),
            root: "ZAPP".to_owned(),
            recursive: true,
            packages_walked: 3,
            packages: vec![
                node("ZAPP", None, 2),
                node("ZAPP_CORE", Some("ZAPP"), 5),
                node("ZAPP_CORE_UI", Some("ZAPP_CORE"), 1),
            ],
            kinds: BTreeMap::from([("CLAS".to_owned(), 6), ("TABL".to_owned(), 2)]),
            packages_failed: vec![PackageFailureOutput {
                package: "ZAPP_LOCKED".to_owned(),
                message: "not authorized".to_owned(),
            }],
        };

        let rendered = render_package_tree_readable(&result);

        assert!(rendered.contains("kinds: CLAS 6, TABL 2"));
        assert!(rendered.contains("\nZAPP  (2 item(s))\n"));
        assert!(rendered.contains("\n  ZAPP_CORE  (5 item(s))\n"));
        assert!(rendered.contains("\n    ZAPP_CORE_UI  (1 item(s))\n"));
        assert!(rendered.contains("! ZAPP_LOCKED - not authorized"));
    }

    #[test]
    fn a_package_whose_parent_was_never_walked_is_still_listed() {
        let result = PackageTreeResultOutput {
            ok: true,
            profile: "de2".to_owned(),
            root: "ZAPP_CORE".to_owned(),
            recursive: false,
            packages_walked: 1,
            packages: vec![node("ZAPP_CORE", Some("ZAPP"), 5)],
            kinds: BTreeMap::new(),
            packages_failed: Vec::new(),
        };

        let rendered = render_package_tree_readable(&result);

        assert!(rendered.contains("(this package only)"));
        assert!(rendered.contains("ZAPP_CORE  (5 item(s))"));
    }

    #[test]
    fn readable_items_output_tabulates_the_items() {
        let result = PackageItemsResultOutput {
            ok: true,
            profile: "de2".to_owned(),
            root: "ZAPP".to_owned(),
            recursive: true,
            total_matching: 9,
            returned: 1,
            offset: 0,
            limit: 1,
            next_offset: Some(1),
            kinds: BTreeMap::from([("CLAS".to_owned(), 9)]),
            packages_failed: Vec::new(),
            items: vec![PackageItemOutput {
                name: "ZCL_SAMPLE".to_owned(),
                kind: "CLAS".to_owned(),
                object_type: "CLAS/OC".to_owned(),
                package: "ZAPP".to_owned(),
                description: Some("Sample class".to_owned()),
                uri: Some("/sap/bc/adt/oo/classes/zcl_sample".to_owned()),
            }],
        };

        let rendered = render_package_items_readable(&result);

        assert!(rendered.contains("items: 1 of 9 (offset 0, limit 1)"));
        assert!(rendered.contains("next offset: 1"));
        assert!(rendered.contains("ZCL_SAMPLE"));
        assert!(rendered.contains("Sample class"));
    }

    #[test]
    fn parses_package_tree_options_from_cli() {
        let cli =
            Cli::try_parse_from(["fractal", "package", "tree", "ZAPP", "--no-recursive"]).unwrap();

        let Command::Package {
            command: PackageCommand::Tree(args),
        } = cli.command
        else {
            panic!("expected package tree command");
        };
        assert_eq!(args.name, "ZAPP");
        assert!(args.no_recursive);
    }

    #[test]
    fn parses_package_items_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "package",
            "items",
            "ZAPP",
            "--recursive",
            "--kind",
            "clas",
            "--object-type",
            "CLAS/OC",
            "--name-substring",
            "TEST",
            "--offset",
            "2",
            "--limit",
            "5",
        ])
        .unwrap();

        let Command::Package {
            command: PackageCommand::Items(args),
        } = cli.command
        else {
            panic!("expected package items command");
        };
        assert_eq!(args.name, "ZAPP");
        assert!(args.recursive);
        assert_eq!(args.kind.as_deref(), Some("clas"));
        assert_eq!(args.object_type.as_deref(), Some("CLAS/OC"));
        assert_eq!(args.name_substring.as_deref(), Some("TEST"));
        assert_eq!(args.offset, 2);
        assert_eq!(args.limit, 5);
    }
}
