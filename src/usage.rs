//! Token usage (design D11–D13, spec token-usage): the [`TokenUsage`]
//! record each review / executive-summary run stores on its history entry,
//! the tolerant JSON readers the engine adapters share, and the on-demand
//! aggregation behind `GET /usage` and `prx usage` ([`compute`]).
//!
//! Every [`TokenUsage`] field is optional: a field is present only when the
//! engine reported it, and a run whose engine reported nothing records no
//! usage at all — unknown is never written as zeros. Like
//! [`crate::insights`], the aggregation does no I/O: callers hand in the
//! history entries.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeDelta, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::history::HistoryEntry;

// ------------------------------------------------------------------ record

/// One run's reported usage (design D11). `None` = not reported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl TokenUsage {
    /// Whether any token count or the cost was reported (model / session
    /// alone are not usage data).
    pub fn has_counts(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.cost_usd.is_some()
    }

    /// `Some(self)` when it carries usage data, else `None` (unknown).
    pub fn known(self) -> Option<TokenUsage> {
        self.has_counts().then_some(self)
    }

    /// Two attempts of one run (a retry, a resume fallback) as one record:
    /// counts and costs add up (a field reported by either side is kept),
    /// the later attempt's model and session win.
    pub fn combine(earlier: Option<TokenUsage>, later: Option<TokenUsage>) -> Option<TokenUsage> {
        let (a, b) = match (earlier, later) {
            (None, b) => return b,
            (a, None) => return a,
            (Some(a), Some(b)) => (a, b),
        };
        fn add<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            }
        }
        Some(TokenUsage {
            input_tokens: add(a.input_tokens, b.input_tokens),
            output_tokens: add(a.output_tokens, b.output_tokens),
            cache_read_tokens: add(a.cache_read_tokens, b.cache_read_tokens),
            cache_write_tokens: add(a.cache_write_tokens, b.cache_write_tokens),
            cost_usd: add(a.cost_usd, b.cost_usd),
            model: b.model.or(a.model),
            session_id: b.session_id.or(a.session_id),
        })
    }

    /// Usage from an engine that reports cached input as a *subset* of its
    /// input count (codex, OpenAI): `input_tokens` becomes the uncached
    /// remainder (saturating) and `cache_read_tokens` the cached part, so
    /// `input_tokens` means uncached input for every engine, as Anthropic
    /// reports it, and sums of input + cache fields never double-count.
    pub fn from_inclusive_input(
        reported_input: Option<u64>,
        cached: Option<u64>,
        output: Option<u64>,
    ) -> TokenUsage {
        TokenUsage {
            input_tokens: reported_input.map(|i| i.saturating_sub(cached.unwrap_or(0))),
            output_tokens: output,
            cache_read_tokens: cached,
            ..Default::default()
        }
    }

    /// Fill the model / session the engine did not report from what prx
    /// knows about the run (the configured model, the captured session).
    pub fn fill(mut self, model: Option<&str>, session: Option<&str>) -> TokenUsage {
        if self.model.is_none() {
            self.model = model.filter(|m| !m.is_empty()).map(str::to_string);
        }
        if self.session_id.is_none() {
            self.session_id = session.filter(|s| !s.is_empty()).map(str::to_string);
        }
        self
    }
}

/// A [`TokenUsage`] field an agent's `usage_output` may map (design D11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageField {
    InputTokens,
    OutputTokens,
    CacheReadTokens,
    CacheWriteTokens,
    CostUsd,
    Model,
    SessionId,
}

/// The JSON reply of an agent's `usage_output` path: a dotted path into
/// nested objects (`usage.input_tokens`). Valid when every segment is
/// non-empty.
pub fn valid_json_path(path: &str) -> bool {
    !path.trim().is_empty() && path.split('.').all(|s| !s.trim().is_empty())
}

// ------------------------------------------------------- tolerant readers

/// Why a reported value could not be used: the whole usage becomes unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch(pub String);

/// `value` at a dotted `path` (`usage.input_tokens`), if every step exists.
pub fn lookup<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |v, key| v.get(key.trim()))
        .filter(|v| !v.is_null())
}

/// A token count: a non-negative integer (an integral float is accepted).
pub fn count_at(value: &Value, path: &str) -> Result<Option<u64>, Mismatch> {
    let Some(v) = lookup(value, path) else {
        return Ok(None);
    };
    if let Some(n) = v.as_u64() {
        return Ok(Some(n));
    }
    match v.as_f64() {
        Some(f) if f >= 0.0 && f.fract() == 0.0 && f <= u64::MAX as f64 => Ok(Some(f as u64)),
        _ => Err(Mismatch(format!("`{path}` is not a token count: {v}"))),
    }
}

/// A cost in USD: a non-negative finite number.
pub fn cost_at(value: &Value, path: &str) -> Result<Option<f64>, Mismatch> {
    let Some(v) = lookup(value, path) else {
        return Ok(None);
    };
    match v.as_f64() {
        Some(f) if f.is_finite() && f >= 0.0 => Ok(Some(f)),
        _ => Err(Mismatch(format!("`{path}` is not a cost: {v}"))),
    }
}

/// A text field (model, session): a non-empty string, or a number.
pub fn text_at(value: &Value, path: &str) -> Result<Option<String>, Mismatch> {
    let Some(v) = lookup(value, path) else {
        return Ok(None);
    };
    match v {
        Value::String(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Value::Number(n) => Ok(Some(n.to_string())),
        _ => Err(Mismatch(format!("`{path}` is not text: {v}"))),
    }
}

/// The JSON values on an agent's stdout, in order: one JSON document, or
/// the JSON objects of a JSON Lines stream (non-JSON lines are skipped).
pub fn stdout_json_values(stdout: &str) -> Vec<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) {
        return vec![v];
    }
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(Value::is_object)
        .collect()
}

/// Apply an agent's `usage_output` mapping (design D11): each field is read
/// from the last JSON value on stdout where its path resolves. Any type
/// mismatch makes the whole usage unknown (logged at debug level).
pub fn parse_mapped(stdout: &str, mapping: &BTreeMap<UsageField, String>) -> Option<TokenUsage> {
    let values = stdout_json_values(stdout);
    let read = || -> Result<TokenUsage, Mismatch> {
        let mut usage = TokenUsage::default();
        for (field, path) in mapping {
            let Some(v) = values.iter().rev().find(|v| lookup(v, path).is_some()) else {
                continue;
            };
            match field {
                UsageField::InputTokens => usage.input_tokens = count_at(v, path)?,
                UsageField::OutputTokens => usage.output_tokens = count_at(v, path)?,
                UsageField::CacheReadTokens => usage.cache_read_tokens = count_at(v, path)?,
                UsageField::CacheWriteTokens => usage.cache_write_tokens = count_at(v, path)?,
                UsageField::CostUsd => usage.cost_usd = cost_at(v, path)?,
                UsageField::Model => usage.model = text_at(v, path)?,
                UsageField::SessionId => usage.session_id = text_at(v, path)?,
            }
        }
        Ok(usage)
    };
    known_or_log(read(), "usage_output")
}

/// `Ok` usage with data, else `None`; a mismatch is logged at debug level.
pub fn known_or_log(result: Result<TokenUsage, Mismatch>, source: &str) -> Option<TokenUsage> {
    match result {
        Ok(usage) => usage.known(),
        Err(Mismatch(why)) => {
            tracing::debug!("ignoring {source} token usage: {why}");
            None
        }
    }
}

/// Compact token count for CLI output: `950`, `12.3k`, `1.2M`.
pub fn format_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        // Up to the last value that still rounds to `999.9k`; from there on
        // the one-decimal `k` form would read `1000.0k`.
        1_000..=999_949 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// A cost for CLI output: `$0.41`, `$0.0042` for sub-cent amounts.
pub fn format_cost(cost: f64) -> String {
    if cost > 0.0 && cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

// ------------------------------------------------------------------- query

/// Bucket size (`day` / `week` = ISO week, Monday start / `month`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    #[default]
    Day,
    Week,
    Month,
}

/// The time window ending today (local time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Window {
    #[serde(rename = "7d")]
    Days7,
    #[default]
    #[serde(rename = "30d")]
    Days30,
    #[serde(rename = "90d")]
    Days90,
    #[serde(rename = "all")]
    All,
}

/// The optional split dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    #[default]
    None,
    Engine,
    Repo,
    Pr,
    Session,
}

macro_rules! token_enum {
    ($ty:ident, $what:literal, [$($variant:ident => $token:literal),+ $(,)?]) => {
        impl $ty {
            pub const ALLOWED: &'static [&'static str] = &[$($token),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($ty::$variant => $token),+
                }
            }
        }

        impl FromStr for $ty {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, String> {
                match s.trim() {
                    $($token => Ok($ty::$variant),)+
                    other => Err(format!(
                        "invalid {} '{other}': expected one of {}",
                        $what,
                        Self::ALLOWED.join(", ")
                    )),
                }
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

token_enum!(Bucket, "bucket", [Day => "day", Week => "week", Month => "month"]);
token_enum!(Window, "window", [Days7 => "7d", Days30 => "30d", Days90 => "90d", All => "all"]);
token_enum!(Group, "group", [
    None => "none", Engine => "engine", Repo => "repo", Pr => "pr", Session => "session",
]);

impl Window {
    /// Days covered, today included; `None` = all history.
    pub fn days(self) -> Option<i64> {
        match self {
            Window::Days7 => Some(7),
            Window::Days30 => Some(30),
            Window::Days90 => Some(90),
            Window::All => None,
        }
    }
}

/// One aggregation request (`GET /usage` / `prx usage`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub bucket: Bucket,
    pub window: Window,
    pub group: Group,
    /// Restrict to one `org/repo`.
    pub repo: Option<String>,
}

impl Query {
    /// Parse the wire parameters; unset (or empty) values take the
    /// defaults. An invalid value names the allowed ones.
    pub fn parse(
        bucket: Option<&str>,
        window: Option<&str>,
        group: Option<&str>,
        repo: Option<&str>,
    ) -> Result<Query, String> {
        fn opt<T: FromStr<Err = String> + Default>(v: Option<&str>) -> Result<T, String> {
            match v.map(str::trim).filter(|v| !v.is_empty()) {
                Some(v) => v.parse(),
                None => Ok(T::default()),
            }
        }
        Ok(Query {
            bucket: opt(bucket)?,
            window: opt(window)?,
            group: opt(group)?,
            repo: repo
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_string),
        })
    }
}

// ----------------------------------------------------------------- results

/// Token totals of one part of a run (the review or the summary).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Sum of reported costs only.
    pub cost_usd: f64,
}

impl Split {
    fn add(&mut self, u: &TokenUsage) {
        // Saturating: an engine reporting an absurd count must not overflow
        // (a panic in debug builds, a wrapped total in release).
        let add = |total: &mut u64, n: Option<u64>| *total = total.saturating_add(n.unwrap_or(0));
        add(&mut self.input_tokens, u.input_tokens);
        add(&mut self.output_tokens, u.output_tokens);
        add(&mut self.cache_read_tokens, u.cache_read_tokens);
        add(&mut self.cache_write_tokens, u.cache_write_tokens);
        self.cost_usd += u.cost_usd.unwrap_or(0.0);
    }
}

/// Aggregated usage: the token fields and cost (review + summary), the run
/// counts, and the review / summary split.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Sum of reported costs only (see `runs_with_cost`).
    pub cost_usd: f64,
    /// Runs with usage data.
    pub runs: u64,
    /// Runs that reported a cost.
    pub runs_with_cost: u64,
    pub review: Split,
    pub summary: Split,
}

impl Totals {
    fn add_run(&mut self, run: &Run) {
        if let Some(u) = run.review {
            self.review.add(u);
        }
        if let Some(u) = run.summary {
            self.summary.add(u);
        }
        let (r, s) = (&self.review, &self.summary);
        self.input_tokens = r.input_tokens.saturating_add(s.input_tokens);
        self.output_tokens = r.output_tokens.saturating_add(s.output_tokens);
        self.cache_read_tokens = r.cache_read_tokens.saturating_add(s.cache_read_tokens);
        self.cache_write_tokens = r.cache_write_tokens.saturating_add(s.cache_write_tokens);
        self.cost_usd = r.cost_usd + s.cost_usd;
        self.runs += 1;
        if run.has_cost() {
            self.runs_with_cost += 1;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Local date of the bucket's first day.
    pub bucket_start: NaiveDate,
    pub totals: Totals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub group: String,
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupTotals {
    pub group: String,
    pub totals: Totals,
}

/// The `GET /usage` body (notes.md pins this shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub bucket: Bucket,
    pub window: Window,
    pub group: Group,
    /// One series per group (`"all"` when ungrouped), with one point per
    /// bucket in the window (zeros for empty buckets).
    pub series: Vec<Series>,
    pub groups: Vec<GroupTotals>,
    pub overall: Totals,
    /// Review runs in scope whose engine reported no usage (including
    /// entries written before usage capture existed).
    pub runs_without_usage: u64,
}

/// One review run with usage data, as the aggregation sees it.
struct Run<'a> {
    date: NaiveDate,
    group: String,
    review: Option<&'a TokenUsage>,
    summary: Option<&'a TokenUsage>,
}

impl Run<'_> {
    fn has_cost(&self) -> bool {
        self.review.is_some_and(|u| u.cost_usd.is_some())
            || self.summary.is_some_and(|u| u.cost_usd.is_some())
    }
}

/// [`compute_in`] in the machine's local time zone.
pub fn compute(entries: &[HistoryEntry], now: DateTime<Utc>, query: &Query) -> Usage {
    compute_in(entries, now, query, &Local)
}

/// Aggregate the usage of every completed / failed review run in `entries`
/// (design D13): local-date buckets in `tz`, split by `query.group`.
pub fn compute_in<Tz: TimeZone>(
    entries: &[HistoryEntry],
    now: DateTime<Utc>,
    query: &Query,
    tz: &Tz,
) -> Usage {
    let local = |ts: &DateTime<Utc>| ts.with_timezone(tz).date_naive();
    let today = local(&now);
    let window_start = query
        .window
        .days()
        .map(|days| today - TimeDelta::try_days(days - 1).unwrap_or_else(TimeDelta::zero));

    let mut runs: Vec<Run> = Vec::new();
    let mut first_date: Option<NaiveDate> = None;
    let mut runs_without_usage = 0u64;
    for entry in entries {
        let (review, summary, agent) = match entry {
            HistoryEntry::ReviewCompleted {
                usage,
                summary_usage,
                agent,
                ..
            } => (usage.as_ref(), summary_usage.as_ref(), agent),
            HistoryEntry::ReviewFailed { usage, agent, .. } => (usage.as_ref(), None, agent),
            // Design B3: an on-demand brief run counts as brief usage (the
            // `summary` split). Without usage it is not a review run, so it
            // never counts toward `runs_without_usage`.
            HistoryEntry::Brief {
                usage: Some(usage),
                agent,
                ..
            } => (None, Some(usage), agent),
            _ => continue,
        };
        if query.repo.as_deref().is_some_and(|r| r != entry.repo()) {
            continue;
        }
        let date = local(&entry.ts());
        if window_start.is_some_and(|start| date < start) || date > today {
            continue;
        }
        first_date = Some(first_date.map_or(date, |d| d.min(date)));
        let review = review.filter(|u| u.has_counts());
        let summary = summary.filter(|u| u.has_counts());
        if review.is_none() && summary.is_none() {
            runs_without_usage += 1;
            continue;
        }
        let engine = agent.as_deref().unwrap_or("unknown");
        let group = match query.group {
            Group::None => "all".to_string(),
            Group::Engine => engine.to_string(),
            Group::Repo => entry.repo().to_string(),
            Group::Pr => format!("{}#{}", entry.repo(), entry.number()),
            // A run without a session ID is a session of its own.
            Group::Session => match review.and_then(|u| u.session_id.as_deref()) {
                Some(session) => format!("{engine}:{session}"),
                None => format!("{engine}:{}", entry.ts().to_rfc3339()),
            },
        };
        runs.push(Run {
            date,
            group,
            review,
            summary,
        });
    }

    // Every bucket from the window's first to today's (for `all`: from the
    // first run's bucket; nothing in scope means no points).
    let buckets: Vec<NaiveDate> = match window_start.or(first_date) {
        Some(start) => {
            let mut out = Vec::new();
            let mut b = bucket_start(start, query.bucket);
            let last = bucket_start(today, query.bucket);
            while b <= last {
                out.push(b);
                b = next_bucket(b, query.bucket);
            }
            out
        }
        None => Vec::new(),
    };

    let mut overall = Totals::default();
    let mut by_group: BTreeMap<String, (Totals, BTreeMap<NaiveDate, Totals>)> = BTreeMap::new();
    if query.group == Group::None {
        by_group.insert("all".to_string(), Default::default());
    }
    for run in &runs {
        overall.add_run(run);
        let (totals, points) = by_group.entry(run.group.clone()).or_default();
        totals.add_run(run);
        points
            .entry(bucket_start(run.date, query.bucket))
            .or_default()
            .add_run(run);
    }

    let mut series = Vec::new();
    let mut groups = Vec::new();
    for (group, (totals, points)) in by_group {
        series.push(Series {
            group: group.clone(),
            points: buckets
                .iter()
                .map(|b| Point {
                    bucket_start: *b,
                    totals: points.get(b).copied().unwrap_or_default(),
                })
                .collect(),
        });
        groups.push(GroupTotals { group, totals });
    }
    Usage {
        bucket: query.bucket,
        window: query.window,
        group: query.group,
        series,
        groups,
        overall,
        runs_without_usage,
    }
}

/// The first day of the bucket holding `date` (ISO weeks start Monday).
pub fn bucket_start(date: NaiveDate, bucket: Bucket) -> NaiveDate {
    match bucket {
        Bucket::Day => date,
        Bucket::Week => {
            date - TimeDelta::try_days(date.weekday().num_days_from_monday() as i64)
                .unwrap_or_else(TimeDelta::zero)
        }
        Bucket::Month => date.with_day(1).unwrap_or(date),
    }
}

fn next_bucket(start: NaiveDate, bucket: Bucket) -> NaiveDate {
    match bucket {
        Bucket::Day => start + TimeDelta::try_days(1).unwrap_or_else(TimeDelta::zero),
        Bucket::Week => start + TimeDelta::try_days(7).unwrap_or_else(TimeDelta::zero),
        Bucket::Month => {
            let (y, m) = match start.month() {
                12 => (start.year() + 1, 1),
                m => (start.year(), m + 1),
            };
            NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(NaiveDate::MAX)
        }
    }
}

// ------------------------------------------------------------------ render

/// The `prx usage` table: one row per non-empty bucket (per group when
/// grouped), the group totals, then the overall total row.
pub fn render(usage: &Usage) -> String {
    let mut out = String::new();
    let label = |s: &str| format!("{s:<12}");
    let row = |name: &str, t: &Totals| {
        format!(
            "{}{:>10} {:>10} {:>11} {:>11} {:>10} {:>6}\n",
            label(name),
            format_tokens(t.input_tokens),
            format_tokens(t.output_tokens),
            format_tokens(t.cache_read_tokens),
            format_tokens(t.cache_write_tokens),
            match t.runs_with_cost {
                0 => "-".to_string(),
                _ => format_cost(t.cost_usd),
            },
            t.runs
        )
    };
    out.push_str(&format!(
        "token usage by {} · window {} · group {}\n",
        usage.bucket, usage.window, usage.group
    ));
    let header = format!(
        "{}{:>10} {:>10} {:>11} {:>11} {:>10} {:>6}\n",
        label(match usage.bucket {
            Bucket::Day => "day",
            Bucket::Week => "week of",
            Bucket::Month => "month",
        }),
        "input",
        "output",
        "cache read",
        "cache write",
        "cost",
        "runs"
    );
    let bucket_label = |d: &NaiveDate| match usage.bucket {
        Bucket::Month => d.format("%Y-%m").to_string(),
        _ => d.format("%Y-%m-%d").to_string(),
    };
    for series in &usage.series {
        if usage.group != Group::None {
            out.push_str(&format!("\n{}\n", series.group));
        }
        out.push_str(&header);
        for point in series.points.iter().filter(|p| p.totals.runs > 0) {
            out.push_str(&row(&bucket_label(&point.bucket_start), &point.totals));
        }
        if usage.group != Group::None {
            let totals = usage
                .groups
                .iter()
                .find(|g| g.group == series.group)
                .map(|g| g.totals)
                .unwrap_or_default();
            out.push_str(&row("total", &totals));
        }
    }
    if usage.group != Group::None {
        out.push('\n');
        out.push_str(&header);
    }
    out.push_str(&row("overall", &usage.overall));
    if usage.runs_without_usage > 0 {
        out.push_str(&format!(
            "{} run(s) without usage data are not in these totals\n",
            usage.runs_without_usage
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Verdict;
    use chrono::FixedOffset;

    fn usage(input: u64, output: u64, session: Option<&str>) -> TokenUsage {
        TokenUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            session_id: session.map(Into::into),
            ..Default::default()
        }
    }

    fn completed(
        ts: &str,
        repo: &str,
        number: u64,
        agent: &str,
        usage: Option<TokenUsage>,
        summary_usage: Option<TokenUsage>,
    ) -> HistoryEntry {
        HistoryEntry::ReviewCompleted {
            ts: ts.parse().unwrap(),
            repo: repo.into(),
            number,
            title: "t".into(),
            author: "a".into(),
            agent: Some(agent.into()),
            verdict: Verdict::Approved,
            blocking_count: 0,
            nit_count: 0,
            duration_seconds: None,
            blocking_texts: Vec::new(),
            brief: None,
            posted: None,
            usage,
            summary_usage,
        }
    }

    fn query(bucket: Bucket, window: Window, group: Group) -> Query {
        Query {
            bucket,
            window,
            group,
            repo: None,
        }
    }

    #[test]
    fn combine_adds_counts_and_keeps_the_later_identity() {
        let a = TokenUsage {
            cost_usd: Some(0.5),
            model: Some("m1".into()),
            ..usage(100, 10, Some("s1"))
        };
        let b = TokenUsage {
            cache_read_tokens: Some(7),
            ..usage(50, 5, Some("s2"))
        };
        let c = TokenUsage::combine(Some(a.clone()), Some(b)).unwrap();
        assert_eq!(c.input_tokens, Some(150));
        assert_eq!(c.output_tokens, Some(15));
        assert_eq!(c.cache_read_tokens, Some(7));
        assert_eq!(c.cost_usd, Some(0.5));
        assert_eq!(c.model.as_deref(), Some("m1"));
        assert_eq!(c.session_id.as_deref(), Some("s2"));
        assert_eq!(TokenUsage::combine(None, None), None);
        assert_eq!(TokenUsage::combine(Some(a.clone()), None), Some(a));
        // Identity alone is not usage data.
        let id_only = TokenUsage {
            model: Some("m".into()),
            ..Default::default()
        };
        assert!(id_only.known().is_none());
    }

    #[test]
    fn mapped_paths_read_nested_fields_and_mismatches_make_usage_unknown() {
        let mapping: BTreeMap<UsageField, String> = [
            (UsageField::InputTokens, "tokens_in".to_string()),
            (UsageField::OutputTokens, "stats.out".to_string()),
            (UsageField::CostUsd, "cost".to_string()),
            (UsageField::Model, "model".to_string()),
        ]
        .into();
        let out = "{\"type\":\"start\",\"model\":\"m-early\"}\nnot json\n{\"tokens_in\":900,\"stats\":{\"out\":120},\"cost\":0.25,\"model\":\"m-1\"}\n";
        let u = parse_mapped(out, &mapping).unwrap();
        assert_eq!(u.input_tokens, Some(900));
        assert_eq!(u.output_tokens, Some(120));
        assert_eq!(u.cost_usd, Some(0.25));
        assert_eq!(u.model.as_deref(), Some("m-1"));
        // Wrong type anywhere: unknown, never zeros.
        assert_eq!(parse_mapped("{\"tokens_in\":\"lots\"}", &mapping), None);
        assert_eq!(parse_mapped("{\"tokens_in\":-3}", &mapping), None);
        // Nothing mapped resolves: unknown.
        assert_eq!(parse_mapped("{\"other\":1}", &mapping), None);
        assert_eq!(parse_mapped("plain text", &mapping), None);
        assert!(valid_json_path("usage.input_tokens"));
        assert!(!valid_json_path("a..b"));
        assert!(!valid_json_path(" "));
    }

    #[test]
    fn query_parsing_lists_the_allowed_values() {
        let q = Query::parse(Some("week"), Some("7d"), Some("session"), Some("")).unwrap();
        assert_eq!(
            q,
            Query {
                bucket: Bucket::Week,
                window: Window::Days7,
                group: Group::Session,
                repo: None
            }
        );
        assert_eq!(
            Query::parse(None, None, None, None).unwrap(),
            Query::default()
        );
        let err = Query::parse(Some("hour"), None, None, None).unwrap_err();
        assert_eq!(
            err,
            "invalid bucket 'hour': expected one of day, week, month"
        );
        assert!(Query::parse(None, Some("1y"), None, None)
            .unwrap_err()
            .contains("7d, 30d, 90d, all"));
        assert!(Query::parse(None, None, Some("author"), None)
            .unwrap_err()
            .contains("none, engine, repo, pr, session"));
    }

    /// Design B3: an on-demand `brief` event's usage lands in the brief
    /// (`summary`) split and the totals; a brief event without usage is
    /// neither a run nor a run without usage.
    #[test]
    fn brief_events_count_as_brief_usage() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let brief = |ts: &str, usage: Option<TokenUsage>| HistoryEntry::Brief {
            ts: ts.parse().unwrap(),
            repo: "o/r".into(),
            number: 4,
            title: "t".into(),
            author: "a".into(),
            agent: Some("cheap".into()),
            brief: usage.as_ref().map(|_| "Adds a cache.".to_string()),
            usage,
        };
        let entries = [
            completed(
                "2026-10-06T10:00:00Z",
                "o/r",
                4,
                "claude",
                Some(usage(1000, 100, None)),
                Some(usage(50, 5, None)),
            ),
            brief("2026-10-06T11:00:00Z", Some(usage(300, 30, None))),
            brief("2026-10-06T12:00:00Z", None),
        ];
        let now: DateTime<Utc> = "2026-10-07T09:00:00Z".parse().unwrap();
        let u = compute_in(
            &entries,
            now,
            &query(Bucket::Day, Window::Days7, Group::Engine),
            &utc,
        );
        assert_eq!(u.overall.review.input_tokens, 1000);
        assert_eq!(u.overall.summary.input_tokens, 350);
        assert_eq!(u.overall.summary.output_tokens, 35);
        assert_eq!(u.overall.input_tokens, 1350);
        assert_eq!(u.overall.runs, 2);
        assert_eq!(u.runs_without_usage, 0);
        let groups: Vec<&str> = u.groups.iter().map(|g| g.group.as_str()).collect();
        assert_eq!(groups, ["cheap", "claude"]);
    }

    /// ISO weeks start on Monday: Sunday 2026-09-27 and Monday 2026-09-28
    /// land in different weeks; every week in the window gets a point.
    #[test]
    fn weekly_buckets_follow_iso_week_boundaries() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let entries = [
            completed(
                "2026-09-27T12:00:00Z",
                "o/r",
                1,
                "claude",
                Some(usage(100, 1, None)),
                None,
            ),
            completed(
                "2026-09-28T12:00:00Z",
                "o/r",
                2,
                "claude",
                Some(usage(200, 2, None)),
                None,
            ),
            completed(
                "2026-10-04T23:00:00Z",
                "o/r",
                3,
                "codex",
                Some(usage(400, 4, None)),
                None,
            ),
        ];
        let now: DateTime<Utc> = "2026-10-07T09:00:00Z".parse().unwrap();
        let u = compute_in(
            &entries,
            now,
            &query(Bucket::Week, Window::Days30, Group::Engine),
            &utc,
        );
        assert_eq!(u.series.len(), 2);
        let claude = &u.series[0];
        assert_eq!(claude.group, "claude");
        let starts: Vec<String> = claude
            .points
            .iter()
            .map(|p| p.bucket_start.to_string())
            .collect();
        // 30 days back from 2026-10-07 is 2026-09-08 (a Tuesday).
        assert_eq!(
            starts,
            [
                "2026-09-07",
                "2026-09-14",
                "2026-09-21",
                "2026-09-28",
                "2026-10-05"
            ]
        );
        assert_eq!(claude.points[2].totals.input_tokens, 100); // Sun 09-27
        assert_eq!(claude.points[3].totals.input_tokens, 200); // Mon 09-28
        assert_eq!(u.series[1].points[3].totals.input_tokens, 400); // codex Sun 10-04
        assert_eq!(u.overall.input_tokens, 700);
        assert_eq!(u.overall.runs, 3);
        assert_eq!(u.groups[0].totals.runs, 2);
    }

    /// Buckets use the local date: 23:30 UTC is already the next day at
    /// UTC+2 and still the same day at UTC-5.
    #[test]
    fn day_buckets_use_the_local_time_zone_around_midnight() {
        let entries = [completed(
            "2026-10-05T23:30:00Z",
            "o/r",
            1,
            "claude",
            Some(usage(10, 1, None)),
            None,
        )];
        let now: DateTime<Utc> = "2026-10-07T12:00:00Z".parse().unwrap();
        let q = query(Bucket::Day, Window::Days7, Group::None);
        let day_of = |tz: FixedOffset| {
            let u = compute_in(&entries, now, &q, &tz);
            assert_eq!(u.series.len(), 1);
            assert_eq!(u.series[0].group, "all");
            assert_eq!(u.series[0].points.len(), 7);
            u.series[0]
                .points
                .iter()
                .find(|p| p.totals.runs == 1)
                .map(|p| p.bucket_start.to_string())
                .unwrap()
        };
        assert_eq!(
            day_of(FixedOffset::east_opt(2 * 3600).unwrap()),
            "2026-10-06"
        );
        assert_eq!(
            day_of(FixedOffset::west_opt(5 * 3600).unwrap()),
            "2026-10-05"
        );
        assert_eq!(day_of(FixedOffset::east_opt(0).unwrap()), "2026-10-05");
    }

    /// Spec "Session rollup": three runs resuming one claude session show
    /// once with the tokens added up and a run count of 3; a run without a
    /// session ID is a session of its own.
    #[test]
    fn session_group_rolls_up_resumed_runs() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let entries = [
            completed(
                "2026-10-01T10:00:00Z",
                "o/r",
                42,
                "claude",
                Some(usage(100, 10, Some("s-1"))),
                None,
            ),
            completed(
                "2026-10-02T10:00:00Z",
                "o/r",
                42,
                "claude",
                Some(usage(200, 20, Some("s-1"))),
                None,
            ),
            completed(
                "2026-10-03T10:00:00Z",
                "o/r",
                42,
                "claude",
                Some(usage(300, 30, Some("s-1"))),
                Some(TokenUsage {
                    cost_usd: Some(0.01),
                    ..usage(5, 1, None)
                }),
            ),
            completed(
                "2026-10-03T11:00:00Z",
                "o/r",
                43,
                "api",
                Some(usage(7, 1, None)),
                None,
            ),
        ];
        let now: DateTime<Utc> = "2026-10-07T12:00:00Z".parse().unwrap();
        let u = compute_in(
            &entries,
            now,
            &query(Bucket::Day, Window::Days30, Group::Session),
            &utc,
        );
        let names: Vec<&str> = u.groups.iter().map(|g| g.group.as_str()).collect();
        assert_eq!(names, ["api:2026-10-03T11:00:00+00:00", "claude:s-1"]);
        let session = &u.groups[1].totals;
        assert_eq!(session.runs, 3);
        assert_eq!(session.input_tokens, 605);
        assert_eq!(session.review.input_tokens, 600);
        assert_eq!(session.summary.input_tokens, 5);
        assert_eq!(session.runs_with_cost, 1);
        assert!((session.cost_usd - 0.01).abs() < 1e-9);
    }

    /// Old entries (no usage fields) and engines that report nothing count
    /// in `runs_without_usage` and contribute no zeros to the totals.
    #[test]
    fn runs_without_usage_are_counted_apart() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let old: HistoryEntry = serde_json::from_str(
            r#"{"event":"review_completed","ts":"2026-10-01T10:00:00Z","repo":"o/r","number":1,"title":"t","author":"a","agent":"pi","verdict":"approved","blocking_count":0,"nit_count":0}"#,
        )
        .unwrap();
        let failed: HistoryEntry = serde_json::from_str(
            r#"{"event":"review_failed","ts":"2026-10-02T10:00:00Z","repo":"o/r","number":2,"title":"t","author":"a","agent":"claude","usage":{"input_tokens":50,"session_id":"s"}}"#,
        )
        .unwrap();
        let other_repo = completed(
            "2026-10-02T10:00:00Z",
            "o/x",
            9,
            "claude",
            Some(usage(1, 1, None)),
            None,
        );
        let ancient = completed("2026-01-02T10:00:00Z", "o/r", 3, "claude", None, None);
        let entries = [old, failed, other_repo, ancient];
        let now: DateTime<Utc> = "2026-10-07T12:00:00Z".parse().unwrap();
        let mut q = query(Bucket::Month, Window::Days30, Group::None);
        q.repo = Some("o/r".into());
        let u = compute_in(&entries, now, &q, &utc);
        assert_eq!(u.runs_without_usage, 1);
        assert_eq!(u.overall.runs, 1);
        assert_eq!(u.overall.input_tokens, 50);
        assert_eq!(u.overall.runs_with_cost, 0);
        let months: Vec<String> = u.series[0]
            .points
            .iter()
            .map(|p| p.bucket_start.to_string())
            .collect();
        assert_eq!(months, ["2026-09-01", "2026-10-01"]);

        // `all` starts at the first run's bucket, the ancient one included.
        q.window = Window::All;
        let u = compute_in(&entries, now, &q, &utc);
        assert_eq!(u.runs_without_usage, 2);
        assert_eq!(u.series[0].points.len(), 10);
        assert_eq!(u.series[0].points[0].bucket_start.to_string(), "2026-01-01");

        // Nothing in scope: the "all" series exists with no points.
        let u = compute_in(
            &[],
            now,
            &query(Bucket::Day, Window::All, Group::None),
            &utc,
        );
        assert_eq!(u.series.len(), 1);
        assert!(u.series[0].points.is_empty());
        assert_eq!(u.overall, Totals::default());
    }

    #[test]
    fn wire_shape_matches_the_pinned_contract() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let entries = [completed(
            "2026-10-06T10:00:00Z",
            "o/r",
            1,
            "claude",
            Some(usage(3, 4, None)),
            None,
        )];
        let now: DateTime<Utc> = "2026-10-07T12:00:00Z".parse().unwrap();
        let u = compute_in(
            &entries,
            now,
            &query(Bucket::Week, Window::Days7, Group::Engine),
            &utc,
        );
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["bucket"], "week");
        assert_eq!(v["window"], "7d");
        assert_eq!(v["group"], "engine");
        assert_eq!(v["series"][0]["group"], "claude");
        assert_eq!(v["series"][0]["points"][0]["bucket_start"], "2026-09-28");
        let totals = &v["series"][0]["points"][1]["totals"];
        for key in [
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "cost_usd",
            "runs",
            "runs_with_cost",
            "review",
            "summary",
        ] {
            assert!(totals.get(key).is_some(), "{key}");
        }
        assert_eq!(totals["review"]["output_tokens"], 4);
        assert_eq!(v["groups"][0]["totals"]["runs"], 1);
        assert_eq!(v["overall"]["input_tokens"], 3);
        assert_eq!(v["runs_without_usage"], 0);
        assert_eq!(serde_json::from_value::<Usage>(v).unwrap(), u);
    }

    #[test]
    fn formatting_is_compact() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(1_240_000), "1.2M");
        assert_eq!(format_cost(0.4123), "$0.41");
        assert_eq!(format_cost(0.0042), "$0.0042");
        assert_eq!(format_cost(0.0), "$0.00");
    }

    fn now() -> DateTime<Utc> {
        "2026-09-30T12:00:00Z".parse().unwrap()
    }

    fn failed(
        ts: &str,
        repo: &str,
        agent: Option<&str>,
        usage: Option<TokenUsage>,
    ) -> HistoryEntry {
        HistoryEntry::ReviewFailed {
            ts: ts.parse().unwrap(),
            repo: repo.into(),
            number: 1,
            title: "t".into(),
            author: "a".into(),
            agent: agent.map(Into::into),
            usage,
        }
    }

    fn with_cost(input: u64, cost: f64) -> TokenUsage {
        TokenUsage {
            cost_usd: Some(cost),
            ..usage(input, 0, None)
        }
    }

    /// The uniform semantics: `input_tokens` is uncached input for every
    /// engine. An engine that counts cached input inside its input total is
    /// split, so input + cache read is what it reported, never more.
    #[test]
    fn inclusive_input_is_split_into_uncached_input_and_cache_read() {
        let u = TokenUsage::from_inclusive_input(Some(1_000), Some(400), Some(50));
        assert_eq!(u.input_tokens, Some(600));
        assert_eq!(u.cache_read_tokens, Some(400));
        assert_eq!(u.output_tokens, Some(50));
        assert_eq!(
            u.input_tokens.unwrap() + u.cache_read_tokens.unwrap(),
            1_000
        );
        // Nothing the engine did not report is invented.
        assert_eq!((u.cache_write_tokens, u.cost_usd), (None, None));
        assert_eq!((u.model.as_deref(), u.session_id.as_deref()), (None, None));

        // No cached count reported: the input stands, cache read stays unknown.
        let u = TokenUsage::from_inclusive_input(Some(1_000), None, None);
        assert_eq!((u.input_tokens, u.cache_read_tokens), (Some(1_000), None));
        assert_eq!(u.output_tokens, None);
        // Everything cached, and a cached count above the total (saturates).
        let u = TokenUsage::from_inclusive_input(Some(400), Some(400), None);
        assert_eq!((u.input_tokens, u.cache_read_tokens), (Some(0), Some(400)));
        let u = TokenUsage::from_inclusive_input(Some(100), Some(400), None);
        assert_eq!((u.input_tokens, u.cache_read_tokens), (Some(0), Some(400)));
        // No input total: unknown input, not "zero minus cached".
        let u = TokenUsage::from_inclusive_input(None, Some(400), Some(5));
        assert_eq!((u.input_tokens, u.cache_read_tokens), (None, Some(400)));
        assert_eq!(
            TokenUsage::from_inclusive_input(None, None, None).known(),
            None
        );
    }

    /// A model or session alone is not usage data; a reported zero is.
    #[test]
    fn known_needs_a_count_or_a_cost() {
        let identity_only = TokenUsage {
            model: Some("m".into()),
            session_id: Some("s".into()),
            ..Default::default()
        };
        assert!(!identity_only.has_counts());
        assert_eq!(identity_only.known(), None);
        assert_eq!(TokenUsage::default().known(), None);

        for reported in [
            TokenUsage {
                input_tokens: Some(0),
                ..Default::default()
            },
            TokenUsage {
                cache_write_tokens: Some(1),
                ..Default::default()
            },
            TokenUsage {
                cost_usd: Some(0.0),
                ..Default::default()
            },
        ] {
            assert_eq!(reported.clone().known(), Some(reported));
        }
    }

    /// `fill` supplies what the engine left out and never replaces what it
    /// reported; empty strings are not a model or a session.
    #[test]
    fn fill_only_supplies_missing_identity() {
        let filled = usage(1, 1, None).fill(Some("configured"), Some("captured"));
        assert_eq!(filled.model.as_deref(), Some("configured"));
        assert_eq!(filled.session_id.as_deref(), Some("captured"));

        let reported = TokenUsage {
            model: Some("reported".into()),
            ..usage(1, 1, Some("own"))
        };
        let kept = reported.fill(Some("configured"), Some("captured"));
        assert_eq!(kept.model.as_deref(), Some("reported"));
        assert_eq!(kept.session_id.as_deref(), Some("own"));

        let blank = usage(1, 1, None).fill(Some(""), Some(""));
        assert_eq!((blank.model, blank.session_id), (None, None));
        let none = usage(1, 1, None).fill(None, None);
        assert_eq!((none.model, none.session_id), (None, None));
    }

    /// One attempt without usage does not erase the other's, and a field
    /// only one attempt reported is kept as reported.
    #[test]
    fn combine_keeps_what_only_one_attempt_reported() {
        assert_eq!(TokenUsage::combine(None, None), None);
        let a = with_cost(100, 0.25);
        assert_eq!(TokenUsage::combine(Some(a.clone()), None), Some(a.clone()));
        assert_eq!(TokenUsage::combine(None, Some(a.clone())), Some(a.clone()));

        let b = TokenUsage {
            cache_write_tokens: Some(9),
            model: Some("m2".into()),
            ..Default::default()
        };
        let both = TokenUsage::combine(Some(a), Some(b)).unwrap();
        assert_eq!(both.input_tokens, Some(100));
        assert_eq!(both.cache_write_tokens, Some(9));
        assert_eq!(both.cache_read_tokens, None, "reported by neither");
        assert_eq!(both.cost_usd, Some(0.25), "one cost is not doubled");
        assert_eq!(both.model.as_deref(), Some("m2"));
    }

    #[test]
    fn readers_accept_only_well_typed_values() {
        let v: Value = serde_json::json!({
            "int": 12, "float": 12.0, "frac": 12.5, "neg": -1, "text": "12",
            "null": null, "flag": true, "huge": 1e30, "zero": 0,
            "model": "  m-1  ", "blank": "   ", "id": 42,
            "nested": {"deep": {"n": 7}}, "list": [1, 2]
        });
        assert_eq!(count_at(&v, "int"), Ok(Some(12)));
        assert_eq!(count_at(&v, "float"), Ok(Some(12)));
        assert_eq!(count_at(&v, "zero"), Ok(Some(0)));
        assert_eq!(count_at(&v, "nested.deep.n"), Ok(Some(7)));
        // Path segments are trimmed, as `valid_json_path` accepts them.
        assert_eq!(count_at(&v, " nested . deep . n "), Ok(Some(7)));
        // Absent, null, and a path through a non-object are "not reported".
        for path in ["missing", "null", "int.deeper", "nested.nope.n", "list.0"] {
            assert_eq!(count_at(&v, path), Ok(None), "{path}");
        }
        for path in ["frac", "neg", "text", "flag", "huge", "nested", "list"] {
            assert!(count_at(&v, path).is_err(), "{path}");
        }

        assert_eq!(cost_at(&v, "frac"), Ok(Some(12.5)));
        assert_eq!(cost_at(&v, "int"), Ok(Some(12.0)));
        assert_eq!(cost_at(&v, "zero"), Ok(Some(0.0)));
        assert_eq!(cost_at(&v, "missing"), Ok(None));
        for path in ["neg", "text", "flag", "nested"] {
            assert!(cost_at(&v, path).is_err(), "{path}");
        }

        assert_eq!(text_at(&v, "model"), Ok(Some("m-1".to_string())));
        assert_eq!(text_at(&v, "id"), Ok(Some("42".to_string())));
        assert_eq!(text_at(&v, "blank"), Ok(None));
        assert_eq!(text_at(&v, "missing"), Ok(None));
        for path in ["flag", "nested", "list"] {
            assert!(text_at(&v, path).is_err(), "{path}");
        }
        // The mismatch says which path and what was there.
        let Mismatch(why) = count_at(&v, "text").unwrap_err();
        assert_eq!(why, "`text` is not a token count: \"12\"");
    }

    /// One JSON document (however it is laid out) is one value; otherwise
    /// each line that is a JSON object counts and everything else is noise.
    #[test]
    fn stdout_is_read_as_one_document_or_as_json_lines() {
        let pretty = "{\n  \"usage\": {\n    \"in\": 5\n  }\n}\n";
        assert_eq!(
            stdout_json_values(pretty),
            [serde_json::json!({"usage": {"in": 5}})]
        );
        let lines = "starting\n{\"a\":1}\n42\n\"text\"\n[1,2]\n  {\"b\":2}  \n{broken\n";
        assert_eq!(
            stdout_json_values(lines),
            [serde_json::json!({"a": 1}), serde_json::json!({"b": 2})]
        );
        assert!(stdout_json_values("").is_empty());
        assert!(stdout_json_values("no json at all\n").is_empty());
    }

    /// Each mapped field comes from the last line where ITS path resolves,
    /// so fields may be spread over several events; identity alone is not
    /// usage.
    #[test]
    fn mapped_fields_resolve_independently_from_the_last_matching_line() {
        let mapping: BTreeMap<UsageField, String> = [
            (UsageField::InputTokens, "usage.in".to_string()),
            (UsageField::OutputTokens, "usage.out".to_string()),
            (UsageField::CacheReadTokens, "usage.cached".to_string()),
            (UsageField::CacheWriteTokens, "usage.written".to_string()),
            (UsageField::SessionId, "session".to_string()),
            (UsageField::Model, "model".to_string()),
        ]
        .into();
        let out = concat!(
            "{\"session\":\"s-9\",\"model\":\"m\"}\n",
            "{\"usage\":{\"in\":10,\"out\":1}}\n",
            "{\"usage\":{\"in\":30,\"cached\":4}}\n",
            "{\"usage\":{\"in\":null,\"written\":2}}\n",
        );
        assert_eq!(
            parse_mapped(out, &mapping),
            Some(TokenUsage {
                input_tokens: Some(30),
                output_tokens: Some(1),
                cache_read_tokens: Some(4),
                cache_write_tokens: Some(2),
                cost_usd: None,
                model: Some("m".into()),
                session_id: Some("s-9".into()),
            })
        );
        assert_eq!(
            parse_mapped("{\"session\":\"s\",\"model\":\"m\"}", &mapping),
            None
        );
        assert_eq!(parse_mapped(out, &BTreeMap::new()), None);
        // One bad field poisons the whole record, good fields included.
        let bad = "{\"usage\":{\"in\":10,\"out\":\"many\"}}";
        assert_eq!(parse_mapped(bad, &mapping), None);
    }

    /// Cost is recorded only when the engine reports it: the sum covers the
    /// reporting runs alone, `runs_with_cost` says how many those were, and
    /// the table prints `-` (not `$0.00`) when none did.
    #[test]
    fn cost_sums_only_the_runs_that_reported_one() {
        let entries = [
            completed(
                "2026-09-29T10:00:00Z",
                "o/r",
                1,
                "claude",
                Some(with_cost(100, 0.25)),
                None,
            ),
            completed(
                "2026-09-29T11:00:00Z",
                "o/r",
                2,
                "pi",
                Some(usage(200, 20, None)),
                None,
            ),
            // The review reported no cost, its brief run did.
            completed(
                "2026-09-29T12:00:00Z",
                "o/r",
                3,
                "pi",
                Some(usage(300, 30, None)),
                Some(with_cost(5, 0.5)),
            ),
        ];
        let q = query(Bucket::Day, Window::Days7, Group::Engine);
        let u = compute_in(&entries, now(), &q, &Utc);
        assert_eq!(u.overall.runs, 3);
        assert_eq!(u.overall.runs_with_cost, 2);
        assert_eq!(u.overall.cost_usd, 0.75);
        assert_eq!(u.overall.review.cost_usd, 0.25);
        assert_eq!(u.overall.summary.cost_usd, 0.5);
        assert_eq!(u.overall.input_tokens, 605);

        let claude_only = [entries[0].clone()];
        let pi_only = [entries[1].clone()];
        let cost_cell = |entries: &[HistoryEntry]| {
            let table = render(&compute_in(entries, now(), &Query::default(), &Utc));
            let overall = table.lines().find(|l| l.starts_with("overall")).unwrap();
            overall.split_whitespace().nth(5).unwrap().to_string()
        };
        assert_eq!(cost_cell(&claude_only), "$0.25");
        assert_eq!(cost_cell(&pi_only), "-");
        // A reported cost of zero is a cost.
        let free = [completed(
            "2026-09-29T10:00:00Z",
            "o/r",
            1,
            "local",
            Some(with_cost(100, 0.0)),
            None,
        )];
        assert_eq!(cost_cell(&free), "$0.00");
    }

    /// The window ends today and counts today: `7d` is today and the six
    /// days before it, by local date. Later-dated entries are out of scope.
    #[test]
    fn window_covers_today_and_the_days_before_it() {
        let run = |ts: &str| completed(ts, "o/r", 1, "pi", Some(usage(1, 0, None)), None);
        let entries = [
            run("2026-09-23T23:59:59Z"), // 7 days back: outside
            run("2026-09-24T00:00:00Z"), // 6 days back: first day inside
            run("2026-09-30T23:59:59Z"), // later today
            run("2026-10-01T00:00:00Z"), // tomorrow
        ];
        let u = compute_in(
            &entries,
            now(),
            &query(Bucket::Day, Window::Days7, Group::None),
            &Utc,
        );
        assert_eq!(u.overall.runs, 2);
        let points = &u.series[0].points;
        assert_eq!(points.len(), 7);
        assert_eq!(points[0].bucket_start.to_string(), "2026-09-24");
        assert_eq!(points[6].bucket_start.to_string(), "2026-09-30");
        assert_eq!(points[0].totals.runs, 1);
        assert_eq!(points[6].totals.runs, 1);

        for (window, days) in [(Window::Days30, 30), (Window::Days90, 90)] {
            let u = compute_in(&[], now(), &query(Bucket::Day, window, Group::None), &Utc);
            assert_eq!(u.series[0].points.len(), days, "{window}");
            assert_eq!(u.overall, Totals::default());
        }

        // The same instants seen from UTC+1: 23:59:59Z on the 23rd is
        // already the 24th there, so that run is inside the window.
        let plus_one = FixedOffset::east_opt(3_600).unwrap();
        let u = compute_in(
            &entries,
            now(),
            &query(Bucket::Day, Window::Days7, Group::None),
            &plus_one,
        );
        assert_eq!(u.overall.runs, 2);
        assert_eq!(u.series[0].points[0].totals.runs, 2);
        assert_eq!(u.series[0].points[6].totals.runs, 0);
    }

    /// Month and week buckets run across a year boundary without a gap.
    #[test]
    fn buckets_continue_across_the_new_year() {
        let entries = [
            completed(
                "2025-11-15T10:00:00Z",
                "o/r",
                1,
                "pi",
                Some(usage(1, 0, None)),
                None,
            ),
            completed(
                "2026-01-31T10:00:00Z",
                "o/r",
                1,
                "pi",
                Some(usage(2, 0, None)),
                None,
            ),
        ];
        let at: DateTime<Utc> = "2026-02-10T12:00:00Z".parse().unwrap();
        let u = compute_in(
            &entries,
            at,
            &query(Bucket::Month, Window::All, Group::None),
            &Utc,
        );
        let months: Vec<(String, u64)> = u.series[0]
            .points
            .iter()
            .map(|p| (p.bucket_start.to_string(), p.totals.input_tokens))
            .collect();
        assert_eq!(
            months,
            [
                ("2025-11-01".to_string(), 1),
                ("2025-12-01".to_string(), 0),
                ("2026-01-01".to_string(), 2),
                ("2026-02-01".to_string(), 0),
            ]
        );

        let day = |s: &str| s.parse::<NaiveDate>().unwrap();
        // 2026-01-01 is a Thursday: its ISO week starts in December.
        assert_eq!(
            bucket_start(day("2026-01-01"), Bucket::Week),
            day("2025-12-29")
        );
        assert_eq!(
            bucket_start(day("2025-12-29"), Bucket::Week),
            day("2025-12-29")
        );
        assert_eq!(
            bucket_start(day("2026-01-04"), Bucket::Week),
            day("2025-12-29")
        );
        assert_eq!(
            bucket_start(day("2026-01-05"), Bucket::Week),
            day("2026-01-05")
        );
        assert_eq!(
            bucket_start(day("2024-02-29"), Bucket::Month),
            day("2024-02-01")
        );
        assert_eq!(
            bucket_start(day("2024-02-29"), Bucket::Day),
            day("2024-02-29")
        );
        assert_eq!(
            next_bucket(day("2025-12-01"), Bucket::Month),
            day("2026-01-01")
        );
        assert_eq!(
            next_bucket(day("2025-12-29"), Bucket::Week),
            day("2026-01-05")
        );
        assert_eq!(
            next_bucket(day("2024-02-28"), Bucket::Day),
            day("2024-02-29")
        );
    }

    /// Every group dimension: runs land in exactly one group, the groups
    /// add up to the overall totals, and an engine-less run is `unknown`.
    #[test]
    fn groups_partition_the_runs_and_add_up_to_overall() {
        let entries = [
            completed(
                "2026-09-28T10:00:00Z",
                "org/a",
                1,
                "claude",
                Some(usage(100, 10, None)),
                Some(usage(7, 1, None)),
            ),
            completed(
                "2026-09-29T10:00:00Z",
                "org/a",
                2,
                "codex",
                Some(usage(200, 20, None)),
                None,
            ),
            failed(
                "2026-09-29T11:00:00Z",
                "org/b",
                Some("claude"),
                Some(usage(50, 0, None)),
            ),
            failed(
                "2026-09-29T12:00:00Z",
                "org/b",
                None,
                Some(usage(5, 0, None)),
            ),
        ];
        let names = |group: Group| -> Vec<(String, u64, u64)> {
            let u = compute_in(
                &entries,
                now(),
                &query(Bucket::Day, Window::Days7, group),
                &Utc,
            );
            assert_eq!(u.overall.runs, 4, "{group}");
            assert_eq!(u.overall.input_tokens, 362, "{group}");
            assert_eq!(
                u.groups.iter().map(|g| g.totals.runs).sum::<u64>(),
                u.overall.runs,
                "{group}"
            );
            assert_eq!(
                u.groups.iter().map(|g| g.totals.input_tokens).sum::<u64>(),
                u.overall.input_tokens,
                "{group}"
            );
            // One series per group, in the same (name) order, each with the
            // full run of buckets.
            let series: Vec<&str> = u.series.iter().map(|s| s.group.as_str()).collect();
            let groups: Vec<&str> = u.groups.iter().map(|g| g.group.as_str()).collect();
            assert_eq!(series, groups, "{group}");
            assert!(u.series.iter().all(|s| s.points.len() == 7), "{group}");
            u.groups
                .iter()
                .map(|g| (g.group.clone(), g.totals.runs, g.totals.input_tokens))
                .collect()
        };
        let row = |name: &str, runs, input| (name.to_string(), runs, input);
        assert_eq!(names(Group::None), [row("all", 4, 362)]);
        assert_eq!(
            names(Group::Engine),
            [
                row("claude", 2, 157),
                row("codex", 1, 200),
                row("unknown", 1, 5)
            ]
        );
        assert_eq!(
            names(Group::Repo),
            [row("org/a", 2, 307), row("org/b", 2, 55)]
        );
        assert_eq!(
            names(Group::Pr),
            [
                row("org/a#1", 1, 107),
                row("org/a#2", 1, 200),
                row("org/b#1", 2, 55)
            ]
        );

        // The review / summary split of the one run that had both.
        let u = compute_in(
            &entries,
            now(),
            &query(Bucket::Day, Window::Days7, Group::Pr),
            &Utc,
        );
        let pr = &u.groups[0].totals;
        assert_eq!((pr.review.input_tokens, pr.summary.input_tokens), (100, 7));
        assert_eq!((pr.review.output_tokens, pr.summary.output_tokens), (10, 1));
        assert_eq!(pr.output_tokens, 11);
    }

    /// The repo filter is an exact match and applies to the unknown-usage
    /// count as well; events that are not review runs are never counted.
    #[test]
    fn repo_filter_is_exact_and_non_run_events_are_ignored() {
        let mut entries = vec![
            completed(
                "2026-09-29T10:00:00Z",
                "org/api",
                1,
                "pi",
                Some(usage(10, 0, None)),
                None,
            ),
            completed(
                "2026-09-29T10:00:00Z",
                "org/api-docs",
                1,
                "pi",
                Some(usage(1_000, 0, None)),
                None,
            ),
            failed("2026-09-29T10:00:00Z", "org/api", Some("pi"), None),
            failed("2026-09-29T10:00:00Z", "org/api-docs", Some("pi"), None),
            // A brief run that reported nothing is not a review run.
            HistoryEntry::Brief {
                ts: "2026-09-29T10:00:00Z".parse().unwrap(),
                repo: "org/api".into(),
                number: 1,
                title: "t".into(),
                author: "a".into(),
                agent: Some("pi".into()),
                brief: None,
                usage: None,
            },
        ];
        for ts in ["2026-09-29T10:00:00Z", "2026-09-29T11:00:00Z"] {
            entries.push(HistoryEntry::PrMerged {
                ts: ts.parse().unwrap(),
                repo: "org/api".into(),
                number: 1,
                title: "t".into(),
                author: "a".into(),
            });
        }
        let mut q = query(Bucket::Day, Window::Days7, Group::None);
        let all = compute_in(&entries, now(), &q, &Utc);
        assert_eq!((all.overall.runs, all.runs_without_usage), (2, 2));

        q.repo = Some("org/api".into());
        let one = compute_in(&entries, now(), &q, &Utc);
        assert_eq!((one.overall.runs, one.runs_without_usage), (1, 1));
        assert_eq!(one.overall.input_tokens, 10);

        q.repo = Some("org".into());
        let none = compute_in(&entries, now(), &q, &Utc);
        assert_eq!((none.overall.runs, none.runs_without_usage), (0, 0));
    }

    /// History lines from older versions: no usage key, an empty usage
    /// object, a usage object with one field, and one with a field this
    /// build does not know.
    #[test]
    fn older_and_newer_usage_records_still_aggregate() {
        let line = |n: u64, usage: &str| -> HistoryEntry {
            serde_json::from_str(&format!(
                r#"{{"event":"review_completed","ts":"2026-09-29T10:00:00Z","repo":"o/r","number":{n},"title":"t","author":"a","verdict":"approved","blocking_count":0,"nit_count":0{usage}}}"#
            ))
            .unwrap()
        };
        let entries = [
            line(1, ""),
            line(2, r#","usage":{}"#),
            line(3, r#","usage":{"model":"m"}"#),
            line(4, r#","usage":{"output_tokens":9}"#),
            line(5, r#","usage":{"input_tokens":4,"reasoning_tokens":99}"#),
        ];
        let u = compute_in(
            &entries,
            now(),
            &query(Bucket::Day, Window::Days7, Group::Engine),
            &Utc,
        );
        assert_eq!(u.runs_without_usage, 3);
        assert_eq!(u.overall.runs, 2);
        assert_eq!((u.overall.input_tokens, u.overall.output_tokens), (4, 9));
        assert_eq!(u.overall.runs_with_cost, 0);
        // These lines predate the engine name too.
        assert_eq!(u.groups.len(), 1);
        assert_eq!(u.groups[0].group, "unknown");
        // And an unknown record is written back without inventing zeros.
        assert_eq!(serde_json::to_string(&TokenUsage::default()).unwrap(), "{}");
    }

    /// Absurd counts saturate instead of overflowing the totals.
    #[test]
    fn totals_saturate() {
        let huge = || {
            Some(TokenUsage {
                input_tokens: Some(u64::MAX),
                output_tokens: Some(u64::MAX - 1),
                ..Default::default()
            })
        };
        let entries = [
            completed("2026-09-29T10:00:00Z", "o/r", 1, "pi", huge(), huge()),
            completed("2026-09-29T11:00:00Z", "o/r", 2, "pi", huge(), None),
        ];
        let u = compute_in(&entries, now(), &Query::default(), &Utc);
        assert_eq!(u.overall.input_tokens, u64::MAX);
        assert_eq!(u.overall.output_tokens, u64::MAX);
        assert_eq!(u.overall.review.input_tokens, u64::MAX);
        assert_eq!(u.overall.runs, 2);
        assert!(render(&u).contains("overall"));
    }

    /// Regression: 999,950 to 999,999 tokens printed as `1000.0k`.
    #[test]
    fn token_counts_switch_units_without_a_four_digit_mantissa() {
        for (n, text) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1.0k"),
            (1_049, "1.0k"),
            (1_050, "1.1k"),
            (999_949, "999.9k"),
            (999_950, "1.0M"),
            (999_999, "1.0M"),
            (1_000_000, "1.0M"),
            (2_500_000_000, "2500.0M"),
        ] {
            assert_eq!(format_tokens(n), text, "{n}");
        }
        assert!(format_tokens(u64::MAX).ends_with('M'));
        assert_eq!(format_cost(0.01), "$0.01");
        assert_eq!(format_cost(0.009), "$0.0090");
        assert_eq!(format_cost(1234.5), "$1234.50");
    }

    /// The CLI flag values (`FromStr`), the wire values (serde), and what
    /// the table header prints (`Display`) are the same tokens.
    #[test]
    fn cli_wire_and_display_tokens_agree() {
        fn check<T>(allowed: &[&str])
        where
            T: FromStr<Err = String> + fmt::Display + Serialize + Copy + PartialEq + fmt::Debug,
            T: serde::de::DeserializeOwned,
        {
            for token in allowed {
                let value: T = token.parse().unwrap();
                assert_eq!(value.to_string(), *token);
                assert_eq!(serde_json::to_value(value).unwrap(), *token);
                let wire: T = serde_json::from_value(serde_json::json!(token)).unwrap();
                assert_eq!(wire, value);
            }
        }
        check::<Bucket>(Bucket::ALLOWED);
        check::<Window>(Window::ALLOWED);
        check::<Group>(Group::ALLOWED);
        assert_eq!(Bucket::ALLOWED, ["day", "week", "month"]);
        assert_eq!(Window::ALLOWED, ["7d", "30d", "90d", "all"]);
        assert_eq!(Group::ALLOWED, ["none", "engine", "repo", "pr", "session"]);
        // Values are case-sensitive; surrounding space is forgiven.
        assert_eq!(" week ".parse::<Bucket>(), Ok(Bucket::Week));
        assert!("Week".parse::<Bucket>().is_err());
        assert_eq!(
            "14d".parse::<Window>().unwrap_err(),
            "invalid window '14d': expected one of 7d, 30d, 90d, all"
        );
        assert_eq!(
            Query::parse(None, None, None, Some("  org/one ")).unwrap(),
            Query {
                repo: Some("org/one".into()),
                ..Query::default()
            }
        );
        assert_eq!(
            Query::default(),
            query(Bucket::Day, Window::Days30, Group::None)
        );
    }

    /// The table: empty buckets are left out, a grouped report has a
    /// section and a `total` row per group and one `overall` row, and the
    /// first column is named after the bucket.
    #[test]
    fn table_lists_non_empty_buckets_group_totals_and_overall() {
        let entries = [
            completed(
                "2026-09-22T10:00:00Z",
                "o/r",
                1,
                "claude",
                Some(usage(1_500, 100, None)),
                None,
            ),
            completed(
                "2026-09-29T10:00:00Z",
                "o/r",
                1,
                "pi",
                Some(usage(500, 50, None)),
                None,
            ),
        ];
        let squeeze = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let table = render(&compute_in(
            &entries,
            now(),
            &query(Bucket::Week, Window::Days30, Group::None),
            &Utc,
        ));
        let lines: Vec<String> = table.lines().map(squeeze).collect();
        assert_eq!(
            lines,
            [
                "token usage by week · window 30d · group none",
                "week of input output cache read cache write cost runs",
                "2026-09-21 1.5k 100 0 0 - 1",
                "2026-09-28 500 50 0 0 - 1",
                "overall 2.0k 150 0 0 - 2",
            ]
        );

        let table = render(&compute_in(
            &entries,
            now(),
            &query(Bucket::Month, Window::Days30, Group::Engine),
            &Utc,
        ));
        let lines: Vec<String> = table.lines().map(squeeze).collect();
        assert_eq!(
            lines,
            [
                "token usage by month · window 30d · group engine",
                "",
                "claude",
                "month input output cache read cache write cost runs",
                "2026-09 1.5k 100 0 0 - 1",
                "total 1.5k 100 0 0 - 1",
                "",
                "pi",
                "month input output cache read cache write cost runs",
                "2026-09 500 50 0 0 - 1",
                "total 500 50 0 0 - 1",
                "",
                "month input output cache read cache write cost runs",
                "overall 2.0k 150 0 0 - 2",
            ]
        );

        // Nothing in scope: the header and a zero overall row, no note.
        let empty = render(&compute_in(&[], now(), &Query::default(), &Utc));
        let lines: Vec<String> = empty.lines().map(squeeze).collect();
        assert_eq!(
            lines,
            [
                "token usage by day · window 30d · group none",
                "day input output cache read cache write cost runs",
                "overall 0 0 0 0 - 0",
            ]
        );
    }
}
