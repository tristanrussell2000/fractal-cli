use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use super::connect;
use crate::{
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::sap::worklist::{InactiveObject, list_inactive_objects};

#[derive(Debug, Serialize)]
pub struct InactiveObjectOutput {
    #[serde(rename = "type")]
    object_type: String,
    name: String,
    /// The `TYPE:NAME` form `edit activate --object` takes.
    object: String,
    uri: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    adt_type: String,
    /// For a class, which of its source streams are staged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    parts: Vec<&'static str>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InactiveGroupOutput {
    /// `None` for objects on no transport, such as those in `$TMP`.
    request: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    objects: Vec<InactiveObjectOutput>,
}

#[derive(Debug, Serialize)]
pub struct EditInactiveOutput {
    ok: bool,
    profile: String,
    count: usize,
    groups: Vec<InactiveGroupOutput>,
}

pub async fn edit_inactive(explicit_profile: Option<&str>) -> Result<EditInactiveOutput, Reported> {
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let objects = list_inactive_objects(&client).await?;
    Ok(map_inactive(profile_name, objects))
}

fn map_inactive(profile: String, objects: Vec<InactiveObject>) -> EditInactiveOutput {
    let count = objects.len();
    let mut by_request: BTreeMap<Option<String>, InactiveGroupOutput> = BTreeMap::new();
    for object in objects {
        let request = object.transport.as_ref().map(|t| t.request.clone());
        let group = by_request
            .entry(request.clone())
            .or_insert_with(|| InactiveGroupOutput {
                request,
                description: None,
                objects: Vec::new(),
            });
        if group.description.is_none() {
            group.description = object
                .transport
                .as_ref()
                .and_then(|t| t.description.clone());
        }
        group.objects.push(InactiveObjectOutput {
            object: format!("{}:{}", object.object_type, object.name),
            object_type: object.object_type,
            name: object.name,
            uri: object.uri,
            adt_type: object.adt_type,
            parts: object.parts.iter().map(|part| part.as_str()).collect(),
            deleted: object.deleted,
            task: object.transport.and_then(|t| t.task),
        });
    }
    // Requests first, then the objects on no transport.
    let mut groups: Vec<InactiveGroupOutput> = by_request.into_values().collect();
    if groups.first().is_some_and(|group| group.request.is_none()) {
        groups.rotate_left(1);
    }
    EditInactiveOutput {
        ok: true,
        profile,
        count,
        groups,
    }
}

pub fn print_edit_inactive(result: &EditInactiveOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    print!("{}", render(result));
}

fn render(result: &EditInactiveOutput) -> String {
    let mut text = String::new();
    if result.count == 0 {
        let _ = writeln!(
            text,
            "Nothing inactive for this user on {}.",
            result.profile
        );
        return text;
    }
    let _ = writeln!(
        text,
        "{} inactive object{} on {}",
        result.count,
        if result.count == 1 { "" } else { "s" },
        result.profile
    );
    for group in &result.groups {
        let _ = writeln!(text);
        match (&group.request, &group.description) {
            (Some(request), Some(description)) => {
                let _ = writeln!(text, "{request}  {description}");
            }
            (Some(request), None) => {
                let _ = writeln!(text, "{request}");
            }
            (None, _) => {
                let _ = writeln!(text, "no transport");
            }
        }
        for object in &group.objects {
            let mut notes = Vec::new();
            if !object.parts.is_empty() {
                notes.push(object.parts.join(", "));
            }
            if object.deleted {
                notes.push("deletion".to_owned());
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join("; "))
            };
            let _ = writeln!(text, "  {}{notes}", object.object);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use fractal::sap::{class_include::ClassInclude, worklist::InactiveTransport};

    fn object(
        object_type: &str,
        name: &str,
        request: Option<&str>,
        parts: Vec<ClassInclude>,
    ) -> InactiveObject {
        InactiveObject {
            object_type: object_type.to_owned(),
            adt_type: format!("{object_type}/X"),
            name: name.to_owned(),
            uri: format!("/sap/bc/adt/x/{}", name.to_ascii_lowercase()),
            parts,
            deleted: false,
            transport: request.map(|request| InactiveTransport {
                request: request.to_owned(),
                task: Some(format!("{request}T")),
                description: Some("Sample request".to_owned()),
            }),
        }
    }

    #[test]
    fn groups_by_request_with_the_untransported_last() {
        let result = map_inactive(
            "dev".to_owned(),
            vec![
                object("CLAS", "ZCL_SAMPLE", None, vec![ClassInclude::Main]),
                object("DDLS", "ZSAMPLE_B", Some("A4HK900200"), Vec::new()),
                object("DDLS", "ZSAMPLE_A", Some("A4HK900123"), Vec::new()),
            ],
        );
        assert_eq!(result.count, 3);
        let requests: Vec<Option<&str>> = result
            .groups
            .iter()
            .map(|group| group.request.as_deref())
            .collect();
        assert_eq!(requests, [Some("A4HK900123"), Some("A4HK900200"), None]);
        assert_eq!(result.groups[0].objects[0].object, "DDLS:ZSAMPLE_A");
        assert_eq!(
            result.groups[0].objects[0].task.as_deref(),
            Some("A4HK900123T")
        );

        let readable = render(&result);
        assert!(
            readable.starts_with("3 inactive objects on dev\n"),
            "{readable}"
        );
        assert!(
            readable.contains("A4HK900123  Sample request\n  DDLS:ZSAMPLE_A\n"),
            "{readable}"
        );
        assert!(
            readable.contains("no transport\n  CLAS:ZCL_SAMPLE  (main)\n"),
            "{readable}"
        );
    }

    #[test]
    fn says_so_when_nothing_is_inactive() {
        let result = map_inactive("dev".to_owned(), Vec::new());
        assert!(result.groups.is_empty());
        assert_eq!(render(&result), "Nothing inactive for this user on dev.\n");
    }
}
