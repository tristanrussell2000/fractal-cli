//! ABAP short dumps (ST22), read through ADT's runtime-error feed.
//!
//! The feed lists dumps newest first. One dump is a small header document —
//! the runtime error, the termination point as an ADT source URI, and a
//! chapter index — plus the dump itself as plain text, which the chapter
//! index's line numbers cut exactly.
//!
//! The feed filters by user only as `$query=and(equals(user,NAME))`: the bare
//! `equals(...)` is refused, and so is every other field tried.
//!
//! **The feed's times are SAP's clock, not necessarily UTC.** They carry a `Z`,
//! but SAP converts from local time with its own daylight-saving rules, and
//! rules that are out of date (a region that stopped changing its clocks) put
//! them an hour off: live, a dump taken at 23:13 UTC was published as 22:13Z.
//! The feed's `from` bound uses the same clock, so a window computed from the
//! caller's clock silently drops dumps. The window is measured back from the
//! feed's own `updated` instead, and applied to the newest entries rather than
//! sent as `from`.

use serde::Serialize;
use thiserror::Error;
use time::{
    Date, Duration, OffsetDateTime, PrimitiveDateTime, Time,
    format_description::well_known::Rfc3339, macros::format_description,
};

use super::{
    client::{SapClient, SapClientError},
    find_attribute_value, find_children, find_non_empty_attribute,
};
use crate::reportable_error::{ReportableError, sap_http_status};

const FEED_PATH: &str = "/sap/bc/adt/runtime/dumps";
const DUMP_PATH: &str = "/sap/bc/adt/runtime/dump";
const FEED_ID_PREFIX: &str = "/sap/bc/adt/vit/runtime/dumps/";

/// The chapters an ABAP developer reads, by SAP's own chapter names; the
/// titles are translated, the names are not. Selected Variables (`kap10`),
/// memory, kernel and system detail are left to `--full`.
const DEVELOPER_CHAPTERS: [&str; 8] = [
    "kap0", "kap1", "kap3", "kap4", "kap28", "kap7", "kap8", "kap11",
];
const CALL_STACK_CHAPTER: &str = "kap11";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpQuery {
    /// `None` for every user.
    pub user: Option<String>,
    /// `None` for no time bound.
    pub since: Option<Since>,
    pub limit: usize,
}

/// How far back to look, on SAP's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// Back from the feed's own time.
    Span(Duration),
    /// From midnight of a date.
    Date(Date),
}

impl Since {
    /// `30m`, `2h`, `7d`, or a `YYYY-MM-DD` date.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeDumpError::InvalidSince`] for anything else.
    pub fn parse(text: &str) -> Result<Self, RuntimeDumpError> {
        let trimmed = text.trim();
        let error = || RuntimeDumpError::InvalidSince(text.to_owned());
        if let Some((amount, unit)) = trimmed
            .char_indices()
            .last()
            .map(|(at, unit)| (&trimmed[..at], unit))
            && let Ok(amount) = amount.parse::<i64>()
        {
            return Ok(Self::Span(match unit {
                'm' => Duration::minutes(amount),
                'h' => Duration::hours(amount),
                'd' => Duration::days(amount),
                _ => return Err(error()),
            }));
        }
        Date::parse(trimmed, format_description!("[year]-[month]-[day]"))
            .map(Self::Date)
            .map_err(|_| error())
    }

    /// The first moment inside the window, as the feed writes times.
    fn cutoff(self, feed_now: OffsetDateTime) -> String {
        let at = match self {
            Self::Span(span) => feed_now.checked_sub(span).unwrap_or(feed_now),
            Self::Date(date) => PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc(),
        };
        at.format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DumpSummary {
    /// The id `dump show` takes, with its spaces kept.
    pub id: String,
    pub time: String,
    pub user: String,
    pub error: String,
    pub program: String,
    pub short_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpList {
    pub dumps: Vec<DumpSummary>,
    /// More dumps match than the limit allowed.
    pub truncated: bool,
    /// The window's start on SAP's clock, when there was a window.
    pub since: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DumpDetail {
    pub id: String,
    pub error: String,
    pub exception: Option<String>,
    pub program: Option<String>,
    pub time: Option<String>,
    pub user: Option<String>,
    pub short_text: Option<String>,
    /// Where the program stopped, as an ADT source URI `edit read` can open.
    pub termination: Option<SourcePosition>,
    pub chapters: Vec<DumpChapter>,
    pub call_stack: Vec<StackFrame>,
    /// Every chapter, rather than the developer ones.
    pub full: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourcePosition {
    pub uri: String,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DumpChapter {
    pub name: String,
    pub title: String,
    pub lines: Vec<String>,
}

/// One row of Active Calls/Events. SAP names the include and the line within
/// it, not an ADT URI, so that is what is reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StackFrame {
    pub level: u32,
    pub kind: String,
    pub name: String,
    pub program: String,
    pub include: String,
    pub line: Option<u32>,
}

#[derive(Debug, Error)]
pub enum RuntimeDumpError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error("SAP returned malformed dump XML: {0}")]
    Parse(#[from] roxmltree::Error),
    #[error("'{0}' is not a dump id")]
    InvalidId(String),
    #[error("there is no dump '{0}'")]
    NotFound(String),
    #[error("no dumps for {0}")]
    NoneFound(String),
    #[error("'{0}' is not a time to search from")]
    InvalidSince(String),
}

impl ReportableError for RuntimeDumpError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(_) => "dump_response_invalid",
            Self::InvalidId(_) => "invalid_dump_id",
            Self::NotFound(_) => "dump_not_found",
            Self::NoneFound(_) => "no_dumps_found",
            Self::InvalidSince(_) => "invalid_since",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(match self {
            Self::Sap(error) => Some(error),
            _ => None,
        })
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(_) => Some("The dump response was not valid ADT XML.".to_owned()),
            Self::InvalidId(_) | Self::NotFound(_) => Some(
                "Copy the id from `fractal dump list`, or use `fractal dump show --latest`."
                    .to_owned(),
            ),
            Self::NoneFound(_) => {
                Some("Look at every user's with `fractal dump list --all-users`.".to_owned())
            }
            Self::InvalidSince(_) => Some(
                "Use minutes, hours or days such as 30m, 2h or 7d, or a date such as 2026-11-17."
                    .to_owned(),
            ),
        }
    }
}

/// Dumps newest first.
///
/// # Errors
///
/// Returns [`RuntimeDumpError`] when the feed cannot be read or parsed.
pub async fn list_dumps(sap: &SapClient, query: &DumpQuery) -> Result<DumpList, RuntimeDumpError> {
    let filter = query
        .user
        .as_deref()
        .map(|user| format!("and(equals(user,{}))", user.trim().to_ascii_uppercase()));
    let top = (query.limit + 1).to_string();
    let mut parameters: Vec<(&str, &str)> = vec![("$top", top.as_str())];
    if let Some(filter) = filter.as_deref() {
        parameters.push(("$query", filter));
    }
    let feed = sap.get_text_with_query(FEED_PATH, &parameters).await?;
    let (dumps, updated) = parse_feed(&feed)?;
    Ok(window(dumps, updated.as_deref(), query))
}

/// The newest `limit` dumps inside the window. Entries arrive newest first,
/// so the first one outside the window ends it.
fn window(dumps: Vec<DumpSummary>, updated: Option<&str>, query: &DumpQuery) -> DumpList {
    let since = query.since.map(|since| {
        let feed_now = updated
            .and_then(|updated| OffsetDateTime::parse(updated, &Rfc3339).ok())
            .unwrap_or_else(OffsetDateTime::now_utc);
        since.cutoff(feed_now)
    });
    let mut dumps: Vec<DumpSummary> = dumps
        .into_iter()
        .take_while(|dump| {
            since
                .as_deref()
                .is_none_or(|since| dump.time.as_str() >= since)
        })
        .collect();
    let truncated = dumps.len() > query.limit;
    dumps.truncate(query.limit);
    DumpList {
        dumps,
        truncated,
        since,
    }
}

/// One dump: its header, the developer chapters (or every chapter), and its
/// call stack.
///
/// # Errors
///
/// Returns [`RuntimeDumpError::InvalidId`] for something that cannot be a dump
/// id, [`RuntimeDumpError::NotFound`] when SAP has no such dump.
pub async fn get_dump(
    sap: &SapClient,
    id: &str,
    full: bool,
) -> Result<DumpDetail, RuntimeDumpError> {
    let id = normalize_id(id)?;
    let path = format!("{DUMP_PATH}/{}", id.replace(' ', "%20"));
    let header = sap.get_text(&path).await.map_err(|error| {
        if error.is_not_found() {
            RuntimeDumpError::NotFound(id.clone())
        } else {
            error.into()
        }
    })?;
    let header = parse_header(&header)?;
    let text = sap.get_text(&format!("{path}/formatted")).await?;
    Ok(build_detail(id, header, &text, full))
}

/// The caller's newest dump.
///
/// # Errors
///
/// Returns [`RuntimeDumpError::NoneFound`] when the user has none.
pub async fn latest_dump(
    sap: &SapClient,
    user: &str,
    full: bool,
) -> Result<DumpDetail, RuntimeDumpError> {
    let list = list_dumps(
        sap,
        &DumpQuery {
            user: Some(user.to_owned()),
            since: None,
            limit: 1,
        },
    )
    .await?;
    let Some(newest) = list.dumps.into_iter().next() else {
        return Err(RuntimeDumpError::NoneFound(user.to_ascii_uppercase()));
    };
    get_dump(sap, &newest.id, full).await
}

/// Accepts an id with its spaces, or with them encoded as `%20`.
fn normalize_id(id: &str) -> Result<String, RuntimeDumpError> {
    let decoded = id.trim().replace("%20", " ");
    let decoded = decoded.strip_prefix(FEED_ID_PREFIX).unwrap_or(&decoded);
    let valid = !decoded.trim().is_empty()
        && decoded
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | ' '));
    if valid {
        Ok(decoded.to_owned())
    } else {
        Err(RuntimeDumpError::InvalidId(id.to_owned()))
    }
}

fn child_text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    find_children(node, name)
        .next()
        .and_then(|child| child.text())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The entries, and the feed's own `updated` time: SAP's clock.
fn parse_feed(feed: &str) -> Result<(Vec<DumpSummary>, Option<String>), RuntimeDumpError> {
    let document = roxmltree::Document::parse(feed)?;
    let updated = child_text(document.root_element(), "updated");
    let mut dumps: Vec<DumpSummary> = Vec::new();
    for entry in find_children(document.root_element(), "entry") {
        let Some(id) = child_text(entry, "id") else {
            continue;
        };
        let Ok(id) = normalize_id(&id) else {
            continue;
        };
        if dumps.iter().any(|known| known.id == id) {
            continue;
        }
        let category = |label: &str| {
            find_children(entry, "category")
                .find(|category| find_attribute_value(*category, "label") == Some(label))
                .and_then(|category| find_non_empty_attribute(category, "term"))
                .unwrap_or_default()
        };
        dumps.push(DumpSummary {
            time: child_text(entry, "published").unwrap_or_default(),
            user: find_children(entry, "author")
                .next()
                .and_then(|author| child_text(author, "name"))
                .unwrap_or_default(),
            error: category("ABAP runtime error"),
            program: category("Terminated ABAP program"),
            short_text: child_text(entry, "title"),
            id,
        });
    }
    Ok((dumps, updated))
}

#[derive(Debug, Default)]
struct DumpHeader {
    error: String,
    exception: Option<String>,
    program: Option<String>,
    time: Option<String>,
    user: Option<String>,
    termination: Option<SourcePosition>,
    /// Name, title and starting line, in line order.
    chapters: Vec<(String, String, usize)>,
}

fn parse_header(xml: &str) -> Result<DumpHeader, RuntimeDumpError> {
    let document = roxmltree::Document::parse(xml)?;
    let root = document.root_element();
    let termination = find_children(root, "links")
        .flat_map(|links| find_children(links, "link"))
        .find(|link| {
            find_attribute_value(*link, "relation")
                .is_some_and(|relation| relation.ends_with("/dump/termination"))
        })
        .and_then(|link| find_non_empty_attribute(link, "uri"))
        .map(|uri| source_position(&uri));
    let mut chapters: Vec<(String, String, usize)> = find_children(root, "chapters")
        .flat_map(|chapters| find_children(chapters, "chapter"))
        .filter_map(|chapter| {
            Some((
                find_non_empty_attribute(chapter, "name")?,
                find_non_empty_attribute(chapter, "title").unwrap_or_default(),
                find_attribute_value(chapter, "line")?.parse().ok()?,
            ))
        })
        .collect();
    chapters.sort_by_key(|(_, _, line)| *line);
    Ok(DumpHeader {
        error: find_non_empty_attribute(root, "error").unwrap_or_default(),
        exception: find_non_empty_attribute(root, "exception"),
        program: find_non_empty_attribute(root, "terminatedProgram"),
        time: find_non_empty_attribute(root, "datetime"),
        user: find_non_empty_attribute(root, "author"),
        termination,
        chapters,
    })
}

/// `adt://SID/sap/bc/adt/…/source/main#start=14` as a path and a line.
fn source_position(uri: &str) -> SourcePosition {
    let path = uri
        .strip_prefix("adt://")
        .and_then(|rest| rest.find('/').map(|slash| &rest[slash..]))
        .unwrap_or(uri);
    let (path, fragment) = path.split_once('#').unwrap_or((path, ""));
    let line = fragment
        .split(';')
        .find_map(|part| part.strip_prefix("start="))
        .and_then(|start| start.split(',').next())
        .and_then(|line| line.parse().ok());
    SourcePosition {
        uri: path.to_owned(),
        line,
    }
}

fn build_detail(id: String, header: DumpHeader, text: &str, full: bool) -> DumpDetail {
    let lines: Vec<&str> = text.lines().collect();
    let mut chapters = Vec::new();
    let mut call_stack = Vec::new();
    for (index, (name, title, start)) in header.chapters.iter().enumerate() {
        let end = header
            .chapters
            .get(index + 1)
            .map_or(lines.len(), |(_, _, next)| next.saturating_sub(1));
        let body: Vec<&str> = lines
            .get(*start..end.min(lines.len()).max(*start))
            .unwrap_or_default()
            .to_vec();
        if name == CALL_STACK_CHAPTER {
            call_stack = parse_call_stack(&body);
        }
        if full || DEVELOPER_CHAPTERS.contains(&name.as_str()) {
            chapters.push(DumpChapter {
                name: name.clone(),
                title: title.clone(),
                lines: chapter_lines(&body),
            });
        }
    }
    let short_text = chapters
        .iter()
        .find(|chapter| chapter.name == "kap0")
        .and_then(|chapter| chapter.lines.first())
        .map(|line| line.trim().to_owned());
    DumpDetail {
        id,
        error: header.error,
        exception: header.exception,
        program: header.program,
        time: header.time,
        user: header.user,
        short_text,
        termination: header.termination,
        chapters,
        call_stack,
        full,
    }
}

/// The text inside the `|` frame, without the rules between blocks or the
/// blank lines at either end.
fn chapter_lines(body: &[&str]) -> Vec<String> {
    let mut lines: Vec<String> = body
        .iter()
        .filter(|line| !line.chars().all(|character| character == '-'))
        .map(|line| {
            let inner = line.strip_prefix('|').unwrap_or(line);
            let inner = inner.strip_suffix('|').unwrap_or(inner);
            inner.trim_end().to_owned()
        })
        .collect();
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Rows come in pairs: level, kind, program, include and line, then the
/// procedure's name on its own line. The columns follow the header, except
/// that a short include lets the line number drift left, so everything after
/// the program column is split on whitespace instead.
fn parse_call_stack(body: &[&str]) -> Vec<StackFrame> {
    let inner: Vec<&str> = body
        .iter()
        .map(|line| {
            let line = line.strip_prefix('|').unwrap_or(line);
            line.strip_suffix('|').unwrap_or(line)
        })
        .collect();
    let Some(header) = inner
        .iter()
        .find(|line| line.trim_start().starts_with("No."))
    else {
        return Vec::new();
    };
    let (Some(kind_at), Some(program_at), Some(include_at)) = (
        header.find("Ty."),
        header.find("Program"),
        header.find("Include"),
    ) else {
        return Vec::new();
    };
    let mut frames = Vec::new();
    let mut rows = inner.iter().peekable();
    while let Some(row) = rows.next() {
        let Some(level) = row
            .get(..kind_at)
            .and_then(|level| level.trim().parse::<u32>().ok())
        else {
            continue;
        };
        let field = |from: usize, to: usize| {
            row.get(from..to.min(row.len()))
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let mut rest = row
            .get(include_at.min(row.len())..)
            .unwrap_or_default()
            .split_whitespace();
        let name = rows
            .next_if(|next| {
                next.get(..kind_at)
                    .is_some_and(|prefix| prefix.trim().is_empty())
                    && !next.trim().is_empty()
            })
            .map(|next| next.trim().to_owned())
            .unwrap_or_default();
        frames.push(StackFrame {
            level,
            kind: field(kind_at, program_at),
            program: field(program_at, include_at),
            include: rest.next().unwrap_or_default().to_owned(),
            line: rest.next().and_then(|line| line.parse().ok()),
            name,
        });
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/dump/feed.xml"
    ));
    const HEADER: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/dump/header.xml"
    ));
    const FORMATTED: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/dump/formatted.txt"
    ));
    const ID: &str = "20261117225656a4h_A4H_00                      DEVUSER1    100        6";

    #[test]
    fn a_feed_entry_names_the_error_program_and_short_text() {
        let (dumps, updated) = parse_feed(FEED).unwrap();
        assert_eq!(updated.as_deref(), Some("2026-11-17T21:57:03Z"));
        assert_eq!(
            dumps,
            [DumpSummary {
                id: ID.to_owned(),
                time: "2026-11-17T21:56:56Z".to_owned(),
                user: "DEVUSER1".to_owned(),
                error: "COMPUTE_INT_ZERODIVIDE".to_owned(),
                program: "ZSAMPLE_DUMPER================CP".to_owned(),
                short_text: Some("Division by 0 (type I or INT8)".to_owned()),
            }]
        );
    }

    #[test]
    fn a_repeated_entry_is_listed_once() {
        let entry_start = FEED.find("<atom:entry").unwrap();
        let entry_end = FEED.find("</atom:entry>").unwrap() + "</atom:entry>".len();
        let entry = &FEED[entry_start..entry_end];
        let doubled = FEED.replacen(entry, &format!("{entry}{entry}"), 1);
        assert_eq!(parse_feed(&doubled).unwrap().0.len(), 1);
    }

    fn summary(time: &str) -> DumpSummary {
        DumpSummary {
            id: time.to_owned(),
            time: time.to_owned(),
            user: "DEVUSER1".to_owned(),
            error: "COMPUTE_INT_ZERODIVIDE".to_owned(),
            program: "ZSAMPLE_DUMPER================CP".to_owned(),
            short_text: None,
        }
    }

    #[test]
    fn the_window_is_measured_on_the_feeds_clock() {
        // The feed says it is 21:57; this machine's clock never enters into it.
        let dumps = vec![
            summary("2026-11-17T21:56:56Z"),
            summary("2026-11-17T21:10:00Z"),
            summary("2026-11-17T20:30:00Z"),
        ];
        let query = |since: &str, limit| DumpQuery {
            user: None,
            since: Some(Since::parse(since).unwrap()),
            limit,
        };
        let hour = window(
            dumps.clone(),
            Some("2026-11-17T21:57:03Z"),
            &query("1h", 20),
        );
        assert_eq!(hour.since.as_deref(), Some("2026-11-17T20:57:03Z"));
        assert_eq!(hour.dumps.len(), 2);
        assert!(!hour.truncated);

        let one = window(dumps.clone(), Some("2026-11-17T21:57:03Z"), &query("1h", 1));
        assert_eq!(one.dumps.len(), 1);
        assert!(one.truncated, "a second dump is inside the window");

        let date = window(
            dumps,
            Some("2026-11-17T21:57:03Z"),
            &query("2026-11-17", 20),
        );
        assert_eq!(date.since.as_deref(), Some("2026-11-17T00:00:00Z"));
        assert_eq!(date.dumps.len(), 3);
    }

    #[test]
    fn since_takes_a_span_or_a_date() {
        assert_eq!(
            Since::parse("30m").unwrap(),
            Since::Span(Duration::minutes(30))
        );
        assert_eq!(
            Since::parse(" 7d ").unwrap(),
            Since::Span(Duration::days(7))
        );
        assert!(matches!(
            Since::parse("2026-11-01").unwrap(),
            Since::Date(_)
        ));
        for bad in ["", "2w", "yesterday", "h"] {
            assert_eq!(
                Since::parse(bad).unwrap_err().code(),
                "invalid_since",
                "{bad}"
            );
        }
    }

    #[test]
    fn the_termination_point_is_an_adt_source_uri_and_line() {
        let header = parse_header(HEADER).unwrap();
        assert_eq!(header.error, "COMPUTE_INT_ZERODIVIDE");
        assert_eq!(header.exception.as_deref(), Some("CX_SY_ZERODIVIDE"));
        assert_eq!(
            header.termination,
            Some(SourcePosition {
                uri: "/sap/bc/adt/oo/classes/zsample_dumper/source/main".to_owned(),
                line: Some(14),
            })
        );
        assert_eq!(header.chapters.first().unwrap().0, "kap0");
    }

    #[test]
    fn shows_the_developer_chapters_and_leaves_out_the_variables() {
        let detail = build_detail(
            ID.to_owned(),
            parse_header(HEADER).unwrap(),
            FORMATTED,
            false,
        );
        let names: Vec<&str> = detail
            .chapters
            .iter()
            .map(|chapter| chapter.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "kap0", "kap1", "kap3", "kap4", "kap28", "kap7", "kap8", "kap11"
            ]
        );
        assert_eq!(
            detail.short_text.as_deref(),
            Some("Division by 0 (type I or INT8)")
        );

        let analysis = &detail.chapters[2];
        assert_eq!(analysis.title, "Error analysis");
        assert!(
            analysis.lines[0].contains("CX_SY_ZERODIVIDE"),
            "{:?}",
            analysis.lines
        );

        // Chapters are cut at the next one in line order, whether or not that
        // one is shown.
        let correct = &detail.chapters[3];
        assert!(
            !correct
                .lines
                .iter()
                .any(|line| line.contains("System environment"))
        );

        let extract = &detail.chapters[6];
        assert!(
            extract
                .lines
                .iter()
                .any(|line| line.contains(">>>>>|    result = 100 / divisor."))
        );
    }

    #[test]
    fn full_keeps_every_chapter_present() {
        let detail = build_detail(
            ID.to_owned(),
            parse_header(HEADER).unwrap(),
            FORMATTED,
            true,
        );
        assert!(
            detail
                .chapters
                .iter()
                .any(|chapter| chapter.name == "kap10")
        );
        assert!(detail.chapters.iter().any(|chapter| chapter.name == "kap9"));
    }

    #[test]
    fn the_call_stack_is_read_row_by_row() {
        let detail = build_detail(
            ID.to_owned(),
            parse_header(HEADER).unwrap(),
            FORMATTED,
            false,
        );
        let stack = &detail.call_stack;
        assert_eq!(stack.len(), 13);
        assert_eq!(
            stack[0],
            StackFrame {
                level: 13,
                kind: "METHOD".to_owned(),
                name: "ZSAMPLE_DUMPER=>DIVIDE".to_owned(),
                program: "ZSAMPLE_DUMPER================CP".to_owned(),
                include: "ZSAMPLE_DUMPER================CM001".to_owned(),
                line: Some(2),
            }
        );
        // A short include lets the line number drift out of its column.
        let last = stack.last().unwrap();
        assert_eq!(last.kind, "MODULE (PBO)");
        assert_eq!((last.include.as_str(), last.line), ("SAPMHTTP", Some(12)));
        assert_eq!(last.name, "%_HTTP_START");
    }

    #[test]
    fn an_id_is_accepted_with_or_without_its_spaces_encoded() {
        let encoded = ID.replace(' ', "%20");
        assert_eq!(normalize_id(&encoded).unwrap(), ID);
        assert_eq!(normalize_id(ID).unwrap(), ID);
        assert_eq!(
            normalize_id(&format!("{FEED_ID_PREFIX}{encoded}")).unwrap(),
            ID
        );
        for bad in ["", "../../oo/classes/x", "20261117?x=1"] {
            assert_eq!(
                normalize_id(bad).unwrap_err().code(),
                "invalid_dump_id",
                "{bad}"
            );
        }
    }
}
