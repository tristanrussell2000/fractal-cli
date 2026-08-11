use std::collections::BTreeMap;

use serde::Serialize;

use crate::cli::PackageItemsArgs;
use crate::{cli::PackageTreeArgs, command_error::CommandError};
use fractal::{
    config, credentials,
    sap::{
        adt::RepositoryKind,
        client::SapClient,
        package::{PackageItemsOptions, get_package_items, get_package_tree},
    },
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
) -> Result<PackageTreeResultOutput, CommandError> {
    let loaded = config::load()?;
    let (profile_name, profile) = config::resolve_profile(&loaded.config, explicit_profile)?;
    let password = credentials::get_password(profile_name)?;
    let mut client = SapClient::new(profile, password)?;
    let recursive = !args.no_recursive;
    let tree = get_package_tree(&mut client, &args.name, recursive).await?;

    Ok(PackageTreeResultOutput {
        ok: true,
        profile: profile_name.to_owned(),
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
) -> Result<PackageItemsResultOutput, CommandError> {
    let kind = args
        .kind
        .as_deref()
        .map(RepositoryKind::parse)
        .transpose()
        .map_err(|error| {
            CommandError::with_hint(
                "invalid_repository_kind",
                error.to_string(),
                "Use a kind such as CLAS, INTF, TABL, PROG, DDLS, or OTHER.",
            )
        })?;
    let loaded = config::load()?;
    let (profile_name, profile) = config::resolve_profile(&loaded.config, explicit_profile)?;
    let password = credentials::get_password(profile_name)?;
    let mut client = SapClient::new(profile, password)?;
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
        profile: profile_name.to_owned(),
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
