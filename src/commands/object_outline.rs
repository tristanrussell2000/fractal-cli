use std::fmt::Write as _;

use serde::Serialize;
use thiserror::Error;

use super::{connect, tabular};
use crate::{
    cli::{DefinitionArgs, OutlineArgs},
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    adt_version::AdtVersion,
    client::SapClient,
    object_source::{ByteRangeOptions, get_source, resolve_source_target},
    outline::{
        Definition, LineRange, Outline, OutlineElement, find_definition, get_outline, symbol_column,
    },
};

#[derive(Debug, Error)]
pub enum DefinitionError {
    #[error("the source has no line {0}")]
    NoSuchLine(u32),
    #[error("'{symbol}' is not on line {line} as a whole word{}", if *occurrence > 1 { format!(" {occurrence} times") } else { String::new() })]
    SymbolNotOnLine {
        symbol: String,
        line: u32,
        occurrence: usize,
    },
}

impl ReportableError for DefinitionError {
    fn code(&self) -> &'static str {
        match self {
            Self::NoSuchLine(_) => "definition_no_such_line",
            Self::SymbolNotOnLine { .. } => "definition_symbol_not_on_line",
        }
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Lines count from 1 in the version read (inactive by default); check --version and --include."
                .to_owned(),
        )
    }
}

#[derive(Debug, Serialize)]
pub struct OutlineOutput {
    ok: bool,
    profile: String,
    uri: String,
    version: &'static str,
    #[serde(flatten)]
    outline: Outline,
}

#[derive(Debug, Serialize)]
pub struct DefinitionOutput {
    ok: bool,
    profile: String,
    /// Where the name was looked up.
    from: Position,
    /// Absent when there is no name at that position.
    definition: Option<Target>,
}

#[derive(Debug, Serialize)]
pub struct Position {
    source_uri: String,
    version: &'static str,
    line: u32,
    /// Counted from 1.
    column: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Target {
    #[serde(flatten)]
    definition: Definition,
    /// The line it points at, in the active version; absent when unreadable.
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

const fn version_name(version: AdtVersion) -> &'static str {
    match version {
        AdtVersion::Active => "active",
        AdtVersion::Inactive => "inactive",
    }
}

pub async fn object_outline(
    explicit_profile: Option<&str>,
    args: &OutlineArgs,
) -> Result<OutlineOutput, Reported> {
    let object = resolve_source_target(&args.uri, None)?;
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let version = args.version.into();
    let outline = get_outline(&client, &object.object_uri, version).await?;
    Ok(OutlineOutput {
        ok: true,
        profile: profile_name,
        uri: object.object_uri,
        version: version_name(version),
        outline,
    })
}

pub async fn object_definition(
    explicit_profile: Option<&str>,
    args: &DefinitionArgs,
) -> Result<DefinitionOutput, Reported> {
    let target = resolve_source_target(&args.uri, args.include.map(Into::into))?;
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let version = args.version.into();
    let source = get_source(&client, &target, version, ByteRangeOptions::default())
        .await?
        .content;
    let line_text = source
        .lines()
        .nth(args.line.saturating_sub(1) as usize)
        .filter(|_| args.line > 0)
        .ok_or(DefinitionError::NoSuchLine(args.line))?;
    let column = match (&args.symbol, args.column) {
        (Some(symbol), _) => {
            symbol_column(line_text, symbol, args.occurrence).ok_or_else(|| {
                DefinitionError::SymbolNotOnLine {
                    symbol: symbol.clone(),
                    line: args.line,
                    occurrence: args.occurrence,
                }
            })?
        }
        (None, Some(column)) => column.saturating_sub(1),
        (None, None) => 0,
    };
    let definition = find_definition(
        &mut client,
        &target.source_uri,
        &source,
        args.line,
        column,
        args.implementation,
    )
    .await?;
    let definition = match definition {
        Some(definition) => Some(Target {
            text: target_line(&client, &definition).await,
            definition,
        }),
        None => None,
    };
    Ok(DefinitionOutput {
        ok: true,
        profile: profile_name,
        from: Position {
            source_uri: target.source_uri,
            version: version_name(version),
            line: args.line,
            column: column + 1,
            symbol: args.symbol.clone(),
        },
        definition,
    })
}

/// The text of the line a definition points at, from the active version.
async fn target_line(client: &SapClient, definition: &Definition) -> Option<String> {
    let object_uri = definition
        .uri
        .split("/source/")
        .next()
        .and_then(|uri| uri.split("/includes/").next())?;
    let include = definition
        .uri
        .rsplit_once("/includes/")
        .and_then(|(_, include)| fractal::sap::class_include::ClassInclude::parse(include));
    let target = resolve_source_target(object_uri, include).ok()?;
    let source = get_source(
        client,
        &target,
        AdtVersion::Active,
        ByteRangeOptions::default(),
    )
    .await
    .ok()?;
    source
        .content
        .lines()
        .nth(definition.line.saturating_sub(1) as usize)
        .map(|line| line.trim().to_owned())
}

pub fn print_object_outline(result: &OutlineOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    print!("{}", render_outline(result));
}

fn render_range(range: Option<&LineRange>) -> String {
    range.map_or_else(String::new, |range| {
        if range.start == range.end {
            format!("{} {}", range.include, range.start)
        } else {
            format!("{} {}-{}", range.include, range.start, range.end)
        }
    })
}

fn render_outline(result: &OutlineOutput) -> String {
    let outline = &result.outline;
    let mut text = String::new();
    let _ = writeln!(
        text,
        "{} ({}, {})",
        outline.name, outline.adt_type, result.version
    );
    let _ = write!(
        text,
        "declaration: {}",
        render_range(outline.declaration.as_ref())
    );
    if outline.implementation.is_some() {
        let _ = write!(
            text,
            "   implementation: {}",
            render_range(outline.implementation.as_ref())
        );
    }
    let _ = writeln!(text);
    let columns = [
        tabular::plain_column("KIND"),
        tabular::plain_column("NAME"),
        tabular::plain_column("VISIBILITY"),
        tabular::plain_column("DECLARATION"),
        tabular::plain_column("IMPLEMENTATION"),
    ];
    let mut rows = Vec::new();
    collect_rows(&outline.elements, "", &mut rows);
    text.push_str(&tabular::render_grid(&columns, &rows));
    text
}

fn collect_rows(elements: &[OutlineElement], indent: &str, rows: &mut Vec<Vec<String>>) {
    for element in elements {
        let mut flags = Vec::new();
        if element.r#static {
            flags.push("static");
        }
        if element.read_only {
            flags.push("read-only");
        }
        let visibility = element.visibility.clone().unwrap_or_default();
        let visibility = if flags.is_empty() {
            visibility
        } else {
            format!("{visibility} {}", flags.join(" "))
                .trim()
                .to_owned()
        };
        rows.push(vec![
            format!("{indent}{}", element.kind),
            element.name.clone(),
            visibility,
            render_range(element.declaration.as_ref()),
            render_range(element.implementation.as_ref()),
        ]);
        collect_rows(&element.elements, &format!("{indent}  "), rows);
    }
}

pub fn print_object_definition(result: &DefinitionOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    let from = &result.from;
    let mut text = String::new();
    let _ = writeln!(
        text,
        "from: {} line {}, column {} ({}){}",
        from.source_uri,
        from.line,
        from.column,
        from.version,
        from.symbol
            .as_deref()
            .map_or_else(String::new, |symbol| format!(": {symbol}"))
    );
    match &result.definition {
        Some(target) => {
            let _ = writeln!(
                text,
                "to:   {} line {}, column {}",
                target.definition.uri, target.definition.line, target.definition.column
            );
            if let Some(line) = &target.text {
                let _ = writeln!(text, "      {line}");
            }
        }
        None => {
            let _ = writeln!(text, "to:   nothing; there is no name at that position");
        }
    }
    print!("{text}");
}
