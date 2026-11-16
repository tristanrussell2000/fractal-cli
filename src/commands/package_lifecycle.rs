use serde::Serialize;

use super::connect;
use crate::{
    cli::{ObjectDeleteArgs, PackageCreateArgs},
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::sap::package_lifecycle::{PackageCreationRequest, create_package, delete_package};

#[derive(Debug, Serialize)]
pub struct PackageCreateOutput {
    ok: bool,
    profile: String,
    status: &'static str,
    name: String,
    uri: String,
    description: Option<String>,
    parent: Option<String>,
    local: bool,
    software_component: Option<String>,
    transport_layer: Option<String>,
    transport: Option<String>,
}

pub async fn package_create(
    explicit_profile: Option<&str>,
    args: &PackageCreateArgs,
) -> Result<PackageCreateOutput, Reported> {
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let request = PackageCreationRequest {
        name: args.name.clone(),
        description: args.description.clone(),
        parent: args.parent.clone(),
        software_component: args.software_component.clone(),
        transport_layer: args.transport_layer.clone(),
        transport: args.transport.clone(),
        responsible: profile.username.clone(),
    };
    let result = create_package(&mut client, &profile.edit_policy(), &request).await?;
    let package = result.package;
    Ok(PackageCreateOutput {
        ok: true,
        profile: profile_name,
        status: "created_verified",
        name: package.name,
        uri: package.uri,
        description: package.description,
        parent: package.parent,
        local: result.local,
        software_component: package.software_component,
        transport_layer: package.transport_layer,
        transport: result.transport,
    })
}

pub fn print_package_create(result: &PackageCreateOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    println!(
        "Created package {}{}, confirmed by reading it back.",
        result.name,
        result
            .parent
            .as_deref()
            .map(|parent| format!(" under {parent}"))
            .unwrap_or_default()
    );
    println!(
        "software component: {}  transport layer: {}  transport: {}",
        result.software_component.as_deref().unwrap_or("-"),
        result.transport_layer.as_deref().unwrap_or("-"),
        result.transport.as_deref().unwrap_or("none (local)")
    );
}

#[derive(Debug, Serialize)]
pub struct PackageDeleteOutput {
    ok: bool,
    profile: String,
    status: &'static str,
    dry_run: bool,
    deleted: bool,
    object_type: &'static str,
    name: String,
    object_uri: String,
    transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

/// `fractal delete --type DEVC`: kept on the one destructive verb, so the
/// harness rule that refuses `fractal delete` covers packages too.
pub async fn package_delete(
    explicit_profile: Option<&str>,
    args: &ObjectDeleteArgs,
) -> Result<PackageDeleteOutput, Reported> {
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let result = delete_package(
        &mut client,
        &profile.edit_policy(),
        &args.name,
        args.transport.as_deref(),
        args.dry_run,
    )
    .await?;
    Ok(PackageDeleteOutput {
        ok: true,
        profile: profile_name,
        status: if result.deleted {
            "deleted_verified"
        } else {
            "dry_run"
        },
        dry_run: args.dry_run,
        deleted: result.deleted,
        object_type: "DEVC",
        name: result.name,
        object_uri: result.uri,
        transport: result.transport,
        warning: super::object_delete::name_still_locked_warning(result.name_still_locked),
    })
}

pub fn print_package_delete(result: &PackageDeleteOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    if let Some(warning) = &result.warning {
        println!("warning: {warning}");
    }
    if result.deleted {
        println!("Deleted package {}, confirmed gone.", result.name);
    } else {
        println!(
            "Dry run: package {} is empty and can be deleted.",
            result.name
        );
    }
}
