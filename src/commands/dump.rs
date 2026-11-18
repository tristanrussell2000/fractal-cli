use std::fmt::Write as _;

use serde::Serialize;

use super::{connect, tabular};
use crate::{
    cli::{DumpListArgs, DumpShowArgs},
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::sap::runtime_dump::{
    DumpDetail, DumpQuery, DumpSummary, Since, get_dump, latest_dump, list_dumps,
};

#[derive(Debug, Serialize)]
pub struct DumpListOutput {
    ok: bool,
    profile: String,
    /// Whose dumps; absent for every user's.
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    /// The window's start on SAP's clock, which the feed's times also use.
    since: String,
    count: usize,
    /// More dumps match than --limit allowed.
    truncated: bool,
    dumps: Vec<DumpListEntry>,
}

#[derive(Debug, Serialize)]
pub struct DumpListEntry {
    #[serde(flatten)]
    dump: DumpSummary,
    /// The command that reads this dump, quoted for a shell.
    show: String,
}

#[derive(Debug, Serialize)]
pub struct DumpShowOutput {
    ok: bool,
    profile: String,
    #[serde(flatten)]
    dump: DumpDetail,
}

pub async fn dump_list(
    explicit_profile: Option<&str>,
    args: &DumpListArgs,
) -> Result<DumpListOutput, Reported> {
    let since = Since::parse(&args.since)?;
    let (profile_name, profile, client) = connect(explicit_profile).await?;
    let user = if args.all_users {
        None
    } else {
        Some(
            args.user
                .as_deref()
                .unwrap_or(&profile.username)
                .trim()
                .to_ascii_uppercase(),
        )
    };
    let list = list_dumps(
        &client,
        &DumpQuery {
            user: user.clone(),
            since: Some(since),
            limit: args.limit,
        },
    )
    .await?;
    Ok(DumpListOutput {
        ok: true,
        profile: profile_name,
        user,
        since: list.since.unwrap_or_default(),
        count: list.dumps.len(),
        truncated: list.truncated,
        dumps: list
            .dumps
            .into_iter()
            .map(|dump| DumpListEntry {
                show: format!("fractal dump show '{}'", dump.id),
                dump,
            })
            .collect(),
    })
}

pub async fn dump_show(
    explicit_profile: Option<&str>,
    args: &DumpShowArgs,
) -> Result<DumpShowOutput, Reported> {
    let (profile_name, profile, client) = connect(explicit_profile).await?;
    let dump = match args.id.as_deref() {
        Some(id) => get_dump(&client, id, args.full).await?,
        None => latest_dump(&client, &profile.username, args.full).await?,
    };
    Ok(DumpShowOutput {
        ok: true,
        profile: profile_name,
        dump,
    })
}

pub fn print_dump_list(result: &DumpListOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    print!("{}", render_list(result));
}

fn render_list(result: &DumpListOutput) -> String {
    let mut text = String::new();
    let whose = result
        .user
        .as_deref()
        .map_or_else(|| "every user".to_owned(), str::to_owned);
    if result.dumps.is_empty() {
        let _ = writeln!(text, "No dumps for {whose} since {}.", result.since);
        return text;
    }
    let _ = writeln!(
        text,
        "{} dump{} for {whose} since {}{}",
        result.count,
        if result.count == 1 { "" } else { "s" },
        result.since,
        if result.truncated {
            " (more match; raise --limit)"
        } else {
            ""
        }
    );
    for entry in &result.dumps {
        let dump = &entry.dump;
        let _ = writeln!(text);
        let _ = writeln!(
            text,
            "{}  {}  {}  {}",
            dump.time, dump.user, dump.error, dump.program
        );
        if let Some(short_text) = &dump.short_text {
            let _ = writeln!(text, "  {short_text}");
        }
        let _ = writeln!(text, "  {}", entry.show);
    }
    text
}

pub fn print_dump_show(result: &DumpShowOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    print!("{}", render_show(&result.dump));
}

fn render_show(dump: &DumpDetail) -> String {
    let mut text = String::new();
    let _ = writeln!(
        text,
        "{}  {}",
        dump.error,
        dump.short_text.as_deref().unwrap_or_default()
    );
    if let Some(exception) = &dump.exception {
        let _ = writeln!(text, "exception: {exception}");
    }
    if let Some(program) = &dump.program {
        let _ = writeln!(text, "program:   {program}");
    }
    if let Some(termination) = &dump.termination {
        let _ = writeln!(
            text,
            "at:        {}{}",
            termination.uri,
            termination
                .line
                .map_or_else(String::new, |line| format!(" line {line}"))
        );
    }
    let _ = writeln!(
        text,
        "when:      {} by {}",
        dump.time.as_deref().unwrap_or("?"),
        dump.user.as_deref().unwrap_or("?")
    );
    for chapter in &dump.chapters {
        if chapter.name == "kap0" {
            continue;
        }
        let _ = writeln!(text);
        let _ = writeln!(text, "{}", chapter.title);
        if chapter.name == "kap11" && !dump.call_stack.is_empty() {
            let columns = [
                tabular::plain_column("LEVEL"),
                tabular::plain_column("KIND"),
                tabular::plain_column("NAME"),
                tabular::plain_column("INCLUDE"),
                tabular::plain_column("LINE"),
            ];
            let rows: Vec<_> = dump
                .call_stack
                .iter()
                .map(|frame| {
                    vec![
                        frame.level.to_string(),
                        frame.kind.clone(),
                        frame.name.clone(),
                        frame.include.clone(),
                        frame.line.map_or_else(String::new, |line| line.to_string()),
                    ]
                })
                .collect();
            text.push_str(&tabular::render_grid(&columns, &rows));
            continue;
        }
        for line in &chapter.lines {
            let _ = writeln!(text, "{line}");
        }
    }
    if !dump.full {
        let _ = writeln!(text);
        let _ = writeln!(
            text,
            "Variables, memory and system chapters left out; pass --full for the whole dump."
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_listed_dump_carries_the_command_that_reads_it() {
        let result = DumpListOutput {
            ok: true,
            profile: "dev".to_owned(),
            user: Some("DEVUSER1".to_owned()),
            since: "2026-11-16T22:00:00Z".to_owned(),
            count: 1,
            truncated: false,
            dumps: vec![DumpListEntry {
                show: "fractal dump show '20261117225656a4h_A4H_00   DEVUSER1    100   6'"
                    .to_owned(),
                dump: DumpSummary {
                    id: "20261117225656a4h_A4H_00   DEVUSER1    100   6".to_owned(),
                    time: "2026-11-17T21:56:56Z".to_owned(),
                    user: "DEVUSER1".to_owned(),
                    error: "COMPUTE_INT_ZERODIVIDE".to_owned(),
                    program: "ZSAMPLE_DUMPER================CP".to_owned(),
                    short_text: Some("Division by 0 (type I or INT8)".to_owned()),
                },
            }],
        };
        let text = render_list(&result);
        assert!(
            text.starts_with("1 dump for DEVUSER1 since 2026-11-16T22:00:00Z\n"),
            "{text}"
        );
        assert!(
            text.contains("  Division by 0 (type I or INT8)\n"),
            "{text}"
        );
        assert!(
            text.contains("  fractal dump show '20261117225656a4h_A4H_00"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_list_says_whose_and_since_when() {
        let result = DumpListOutput {
            ok: true,
            profile: "dev".to_owned(),
            user: None,
            since: "2026-11-16T22:00:00Z".to_owned(),
            count: 0,
            truncated: false,
            dumps: Vec::new(),
        };
        assert_eq!(
            render_list(&result),
            "No dumps for every user since 2026-11-16T22:00:00Z.\n"
        );
    }
}
