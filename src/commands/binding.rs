use std::fmt::Write as _;

use serde::Serialize;

use super::connect;
use crate::{
    cli::{BindingGetArgs, BindingPublishArgs, BindingShowArgs},
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::sap::service_binding::{
    BindingService, PublishOperation, PublishRequest, PublishResult, PublishStatus,
    SapPublishMessage, ServiceDetails, get_from_service, publish_service_binding,
    show_service_binding,
};

#[derive(Debug, Serialize)]
pub struct EntitySetOutput {
    name: String,
    is_root: bool,
}

#[derive(Debug, Serialize)]
pub struct ServiceOutput {
    service_name: String,
    service_version: String,
    service_definition: String,
    published: bool,
    service_url: Option<String>,
    annotation_url: Option<String>,
    entity_sets: Vec<EntitySetOutput>,
}

impl ServiceOutput {
    fn new(service: BindingService, details: ServiceDetails) -> Self {
        Self {
            service_name: service.service_name,
            service_version: service.service_version,
            service_definition: service.service_definition,
            published: details.published,
            service_url: details.service_url,
            annotation_url: details.annotation_url,
            entity_sets: details
                .entity_sets
                .into_iter()
                .map(|set| EntitySetOutput {
                    name: set.name,
                    is_root: set.is_root,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BindingShowOutput {
    ok: bool,
    profile: String,
    name: String,
    object_uri: String,
    odata_version: &'static str,
    /// The binding document's own flag. It can disagree with the registration
    /// reported per service, which is the one that holds.
    binding_published_flag: Option<bool>,
    services: Vec<ServiceOutput>,
}

pub async fn binding_show(
    explicit_profile: Option<&str>,
    args: &BindingShowArgs,
) -> Result<BindingShowOutput, Reported> {
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let result = show_service_binding(&client, &args.name).await?;

    Ok(BindingShowOutput {
        ok: true,
        profile: profile_name,
        name: result.name,
        object_uri: result.object_uri,
        odata_version: result.protocol.as_str(),
        binding_published_flag: result.binding_published_flag,
        services: result
            .services
            .into_iter()
            .map(|state| ServiceOutput::new(state.service, state.details))
            .collect(),
    })
}

pub fn print_binding_show(result: &BindingShowOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    let mut text = format!("{} (OData {})\n", result.name, result.odata_version);
    for service in &result.services {
        write_service(&mut text, service);
    }
    let registered = result.services.iter().any(|service| service.published);
    if let Some(flag) = result.binding_published_flag
        && flag != registered
    {
        let _ = writeln!(
            text,
            "\nNote: the binding's own flag says published={flag}, which disagrees with the registration above."
        );
    }
    print!("{text}");
}

fn write_service(text: &mut String, service: &ServiceOutput) {
    let _ = writeln!(
        text,
        "  {} {} from {}: {}",
        service.service_name,
        service.service_version,
        service.service_definition,
        if service.published {
            "published"
        } else {
            "not published"
        }
    );
    if let Some(url) = &service.service_url {
        let _ = writeln!(text, "    url: {url}");
    }
    if !service.entity_sets.is_empty() {
        let sets: Vec<String> = service
            .entity_sets
            .iter()
            .map(|set| {
                if set.is_root {
                    format!("{} (root)", set.name)
                } else {
                    set.name.clone()
                }
            })
            .collect();
        let _ = writeln!(text, "    entity sets: {}", sets.join(", "));
    }
}

#[derive(Debug, Serialize)]
pub struct SapMessageOutput {
    severity: String,
    short_text: String,
    long_text: Option<String>,
}

impl From<SapPublishMessage> for SapMessageOutput {
    fn from(message: SapPublishMessage) -> Self {
        Self {
            severity: message.severity,
            short_text: message.short_text,
            long_text: message.long_text,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BindingPublishOutput {
    ok: bool,
    profile: String,
    operation: &'static str,
    /// `published_verified` / `unpublished_verified`, `already_published` /
    /// `already_unpublished`, or `dry_run`.
    status: String,
    /// Whether anything was sent to SAP.
    sent: bool,
    name: String,
    object_uri: String,
    odata_version: &'static str,
    request_path: String,
    sap_message: Option<SapMessageOutput>,
    service: ServiceOutput,
}

pub async fn binding_publish(
    explicit_profile: Option<&str>,
    args: &BindingPublishArgs,
    operation: PublishOperation,
) -> Result<BindingPublishOutput, Reported> {
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let request = PublishRequest {
        operation,
        name: args.name.clone(),
        service_version: args.service_version.clone(),
        dry_run: args.dry_run,
    };
    let result = publish_service_binding(&mut client, &profile.edit_policy(), &request).await?;
    Ok(map_publish_result(profile_name, result))
}

fn map_publish_result(profile: String, result: PublishResult) -> BindingPublishOutput {
    let state = match result.operation {
        PublishOperation::Publish => "published",
        PublishOperation::Unpublish => "unpublished",
    };
    let status = match result.status {
        PublishStatus::Verified => format!("{state}_verified"),
        PublishStatus::AlreadyDone => format!("already_{state}"),
        PublishStatus::WouldSend => "dry_run".to_owned(),
    };
    BindingPublishOutput {
        ok: true,
        profile,
        operation: result.operation.as_str(),
        status,
        sent: result.status == PublishStatus::Verified,
        name: result.name,
        object_uri: result.object_uri,
        odata_version: result.protocol.as_str(),
        request_path: result.request_path,
        sap_message: result.sap_message.map(Into::into),
        service: ServiceOutput::new(result.service, result.details_after),
    }
}

pub fn print_binding_publish(result: &BindingPublishOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    let mut text = match result.status.as_str() {
        "dry_run" => format!(
            "Dry run: would {} {} via POST {}\n",
            result.operation, result.name, result.request_path
        ),
        status if status.starts_with("already_") => format!(
            "{} is {}; nothing was sent.\n",
            result.name,
            status.trim_start_matches("already_")
        ),
        _ => format!(
            "{} {} (OData {}), confirmed by reading it back.\n",
            result.name,
            if result.operation == "publish" {
                "published"
            } else {
                "unpublished"
            },
            result.odata_version
        ),
    };
    if let Some(message) = &result.sap_message {
        let _ = writeln!(text, "SAP: {}", message.short_text);
    }
    write_service(&mut text, &result.service);
    print!("{text}");
}

#[derive(Debug, Serialize)]
pub struct BindingGetOutput {
    ok: bool,
    profile: String,
    name: String,
    odata_version: &'static str,
    service_version: String,
    /// The path that was read: the registered service root plus the caller's.
    url: String,
    status: u16,
    content_type: Option<String>,
    /// Parsed when the service answered JSON; the text as sent otherwise
    /// (`$metadata`, Atom).
    body: serde_json::Value,
    #[serde(skip)]
    raw_body: String,
}

pub async fn binding_get(
    explicit_profile: Option<&str>,
    args: &BindingGetArgs,
) -> Result<BindingGetOutput, Reported> {
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let result = get_from_service(
        &client,
        &args.name,
        args.service_version.as_deref(),
        &args.path,
    )
    .await?;
    let is_json = result
        .content_type
        .as_deref()
        .is_some_and(|content_type| content_type.contains("json"));
    let body = if is_json {
        serde_json::from_str(&result.body)
            .unwrap_or_else(|_| serde_json::Value::String(result.body.clone()))
    } else {
        serde_json::Value::String(result.body.clone())
    };
    Ok(BindingGetOutput {
        ok: true,
        profile: profile_name,
        name: result.name,
        odata_version: result.protocol.as_str(),
        service_version: result.service.service_version,
        url: result.url,
        status: result.status,
        content_type: result.content_type,
        body,
        raw_body: result.body,
    })
}

pub fn print_binding_get(result: &BindingGetOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    println!("{}", result.raw_body);
}
