//! The streaming analyzer: one call per line in, one [`Summary`] out.

use std::collections::HashMap;

use serde::{Serialize, Serializer};

use crate::histogram::{self, Histogram};
use crate::json::{self, Scalar};
use crate::level::{self, Level};
use crate::mask;
use crate::timestamp;

/// Only this many bytes of a line (or JSON message) go into its template.
pub const MAX_TEMPLATE_INPUT: usize = 1024;
/// Example lines are cut to this many characters.
pub const MAX_EXAMPLE_CHARS: usize = 200;
/// Version of the JSON output format.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct Options {
    /// How many templates to report.
    pub top: usize,
    /// Keep only lines at this level or above.
    pub min_level: Option<Level>,
    /// Keep only lines at or after this epoch second.
    pub since: Option<i64>,
    /// Keep only lines before this epoch second.
    pub until: Option<i64>,
    /// Histogram bucket in seconds; `None` picks one automatically.
    pub bucket: Option<i64>,
    /// Year for timestamps that do not carry one.
    pub year: i32,
    /// Cap on distinct templates held in memory.
    pub max_templates: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            top: 10,
            min_level: None,
            since: None,
            until: None,
            bucket: None,
            year: 1970,
            max_templates: 10_000,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct Totals {
    /// Lines read, including blank and filtered ones.
    pub lines: u64,
    /// Bytes read, after decompression.
    pub bytes: u64,
    /// Lines that were empty or whitespace only.
    pub blank: u64,
    /// Lines that passed the filters and are counted everywhere below.
    pub matched: u64,
    /// Non-blank lines removed by `--level`, `--since` or `--until`.
    pub filtered_out: u64,
    /// Non-blank lines in which a timestamp was found.
    pub with_timestamp: u64,
    /// Lines parsed as JSON objects.
    pub json_lines: u64,
    /// Lines that start with `{` but are not valid JSON objects.
    pub malformed_json: u64,
    /// Lines with bytes that are not valid UTF-8.
    pub invalid_utf8: u64,
    /// Lines longer than the reader's line limit.
    pub truncated: u64,
    /// Templates held when the input ended.
    pub distinct_templates: u64,
    /// Matched lines whose template was new after the cap was reached.
    pub template_overflow_lines: u64,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct LevelCounts {
    pub fatal: u64,
    pub error: u64,
    pub warn: u64,
    pub info: u64,
    pub debug: u64,
    pub trace: u64,
    pub unknown: u64,
}

impl LevelCounts {
    pub fn get(&self, level: Level) -> u64 {
        match level {
            Level::Fatal => self.fatal,
            Level::Error => self.error,
            Level::Warn => self.warn,
            Level::Info => self.info,
            Level::Debug => self.debug,
            Level::Trace => self.trace,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TimeRange {
    #[serde(serialize_with = "ser_ts")]
    pub first: i64,
    #[serde(serialize_with = "ser_ts")]
    pub last: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BucketOut {
    #[serde(serialize_with = "ser_ts")]
    pub start: i64,
    pub count: u64,
    pub errors: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HistogramOut {
    pub bucket_seconds: i64,
    /// Non-empty buckets in time order.
    pub buckets: Vec<BucketOut>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TemplateOut {
    pub template: String,
    pub count: u64,
    /// Fraction of matched lines, 0 to 1.
    pub share: f64,
    /// Level of the first line with this template.
    pub level: Option<Level>,
    #[serde(serialize_with = "ser_opt_ts")]
    pub first_seen: Option<i64>,
    #[serde(serialize_with = "ser_opt_ts")]
    pub last_seen: Option<i64>,
    /// 1-based line numbers across all inputs.
    pub first_line: u64,
    pub last_line: u64,
    pub example: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BurstOut {
    #[serde(serialize_with = "ser_ts")]
    pub start: i64,
    #[serde(serialize_with = "ser_ts")]
    pub end: i64,
    pub errors: u64,
    pub lines: u64,
    pub peak_errors: u64,
    pub baseline_errors: u64,
}

/// Everything the reports are rendered from.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub version: u32,
    pub inputs: Vec<String>,
    pub totals: Totals,
    pub time_range: Option<TimeRange>,
    pub levels: LevelCounts,
    pub histogram: Option<HistogramOut>,
    pub templates: Vec<TemplateOut>,
    pub bursts: Vec<BurstOut>,
}

fn ser_ts<S: Serializer>(ts: &i64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&timestamp::format_rfc3339(*ts))
}

fn ser_opt_ts<S: Serializer>(ts: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match ts {
        Some(ts) => ser_ts(ts, s),
        None => s.serialize_none(),
    }
}

#[derive(Debug)]
struct TemplateStats {
    count: u64,
    first_line: u64,
    last_line: u64,
    first_seen: Option<i64>,
    last_seen: Option<i64>,
    level: Option<Level>,
    example: String,
}

#[derive(Debug)]
pub struct Analyzer {
    opts: Options,
    totals: Totals,
    levels: LevelCounts,
    histogram: Histogram,
    templates: HashMap<String, TemplateStats>,
    scratch: String,
}

/// The longest prefix of `s` that is at most `max` bytes.
fn prefix(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn example_of(line: &str) -> String {
    match line.char_indices().nth(MAX_EXAMPLE_CHARS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

impl Analyzer {
    pub fn new(opts: Options) -> Self {
        Analyzer {
            histogram: Histogram::new(opts.bucket),
            opts,
            totals: Totals::default(),
            levels: LevelCounts::default(),
            templates: HashMap::new(),
            scratch: String::new(),
        }
    }

    /// Records bytes consumed from the input, for the totals.
    pub fn add_bytes(&mut self, n: u64) {
        self.totals.bytes += n;
    }

    /// Feeds one line, without its line terminator.
    pub fn feed(&mut self, raw: &[u8], truncated: bool) {
        self.totals.lines += 1;
        self.totals.truncated += u64::from(truncated);
        if raw.iter().all(u8::is_ascii_whitespace) {
            self.totals.blank += 1;
            return;
        }
        let text = String::from_utf8_lossy(raw);
        if matches!(text, std::borrow::Cow::Owned(_)) {
            self.totals.invalid_utf8 += 1;
        }
        let line: &str = &text;

        let mut ts = None;
        let mut level = None;
        let mut is_json = false;
        if line.trim_start().starts_with('{') {
            match json::parse(line) {
                Ok(parsed) => {
                    is_json = true;
                    self.totals.json_lines += 1;
                    ts = match &parsed.timestamp {
                        Some(Scalar::Str(s)) => timestamp::parse_whole(s, self.opts.year),
                        Some(Scalar::Num(n)) => timestamp::from_number(*n),
                        _ => None,
                    };
                    level = match &parsed.level {
                        Some(Scalar::Str(s)) => Level::from_word(s.trim().as_bytes()),
                        Some(Scalar::Num(n)) => Level::from_number(*n),
                        _ => None,
                    };
                    match &parsed.message {
                        Some(message) => {
                            let head = prefix(message, MAX_TEMPLATE_INPUT);
                            mask::mask_into(head, None, &mut self.scratch);
                            if head.len() < message.len() {
                                self.scratch.push_str(" <truncated>");
                            }
                        }
                        None => {
                            self.scratch.clear();
                            self.scratch.push_str("{json: ");
                            self.scratch
                                .push_str(prefix(&parsed.keys, MAX_TEMPLATE_INPUT));
                            self.scratch.push('}');
                        }
                    }
                }
                Err(_) => self.totals.malformed_json += 1,
            }
        }
        let mut span = None;
        if !is_json {
            let found = timestamp::find(line.as_bytes(), self.opts.year);
            span = found.map(|f| (f.start, f.end));
            ts = found.map(|f| f.epoch);
            level = level::detect(line.as_bytes(), span);
        }

        self.totals.with_timestamp += u64::from(ts.is_some());
        if !self.passes(ts, level) {
            self.totals.filtered_out += 1;
            return;
        }
        if !is_json {
            let head = prefix(line, MAX_TEMPLATE_INPUT);
            let span = span.filter(|s| s.1 <= head.len());
            mask::mask_into(head, span, &mut self.scratch);
            if head.len() < line.len() {
                self.scratch.push_str(" <truncated>");
            }
        }
        self.totals.matched += 1;
        match level {
            Some(Level::Fatal) => self.levels.fatal += 1,
            Some(Level::Error) => self.levels.error += 1,
            Some(Level::Warn) => self.levels.warn += 1,
            Some(Level::Info) => self.levels.info += 1,
            Some(Level::Debug) => self.levels.debug += 1,
            Some(Level::Trace) => self.levels.trace += 1,
            None => self.levels.unknown += 1,
        }
        if let Some(ts) = ts {
            self.histogram.add(ts, level.is_some_and(Level::is_error));
        }

        let line_no = self.totals.lines;
        if let Some(stats) = self.templates.get_mut(self.scratch.as_str()) {
            stats.count += 1;
            stats.last_line = line_no;
            if let Some(ts) = ts {
                stats.first_seen = Some(stats.first_seen.map_or(ts, |t| t.min(ts)));
                stats.last_seen = Some(stats.last_seen.map_or(ts, |t| t.max(ts)));
            }
        } else if self.templates.len() < self.opts.max_templates {
            self.templates.insert(
                self.scratch.clone(),
                TemplateStats {
                    count: 1,
                    first_line: line_no,
                    last_line: line_no,
                    first_seen: ts,
                    last_seen: ts,
                    level,
                    example: example_of(line),
                },
            );
        } else {
            self.totals.template_overflow_lines += 1;
        }
    }

    fn passes(&self, ts: Option<i64>, level: Option<Level>) -> bool {
        if let Some(min) = self.opts.min_level {
            if level.is_none_or(|l| l < min) {
                return false;
            }
        }
        if self.opts.since.is_some() || self.opts.until.is_some() {
            let Some(ts) = ts else { return false };
            if self.opts.since.is_some_and(|since| ts < since)
                || self.opts.until.is_some_and(|until| ts >= until)
            {
                return false;
            }
        }
        true
    }

    /// Ends the stream and builds the summary.
    pub fn finish(mut self, inputs: Vec<String>) -> Summary {
        self.totals.distinct_templates = self.templates.len() as u64;
        let matched = self.totals.matched;

        let mut ranked: Vec<(String, TemplateStats)> = self.templates.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.count
                .cmp(&a.1.count)
                .then(a.1.first_line.cmp(&b.1.first_line))
        });
        ranked.truncate(self.opts.top);
        let templates = ranked
            .into_iter()
            .map(|(template, s)| TemplateOut {
                template,
                count: s.count,
                share: s.count as f64 / matched as f64,
                level: s.level,
                first_seen: s.first_seen,
                last_seen: s.last_seen,
                first_line: s.first_line,
                last_line: s.last_line,
                example: s.example,
            })
            .collect();

        let time_range = self
            .histogram
            .range()
            .map(|(first, last)| TimeRange { first, last });
        let buckets = self.histogram.finish();
        let size = self.histogram.bucket_seconds();
        let bursts = histogram::bursts(&buckets, size)
            .into_iter()
            .map(|b| BurstOut {
                start: b.start,
                end: b.end,
                errors: b.errors,
                lines: b.lines,
                peak_errors: b.peak_errors,
                baseline_errors: b.baseline_errors,
            })
            .collect();
        let histogram = (!buckets.is_empty()).then(|| HistogramOut {
            bucket_seconds: size,
            buckets: buckets
                .into_iter()
                .map(|b| BucketOut {
                    start: b.start,
                    count: b.count,
                    errors: b.errors,
                })
                .collect(),
        });

        Summary {
            version: SCHEMA_VERSION,
            inputs,
            totals: self.totals,
            time_range,
            levels: self.levels,
            histogram,
            templates,
            bursts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(opts: Options, lines: &[&str]) -> Summary {
        let mut a = Analyzer::new(opts);
        for line in lines {
            a.add_bytes(line.len() as u64 + 1);
            a.feed(line.as_bytes(), false);
        }
        a.finish(vec!["test".into()])
    }

    fn opts() -> Options {
        Options {
            year: 2024,
            ..Options::default()
        }
    }

    #[test]
    fn groups_lines_into_templates() {
        let s = run(
            opts(),
            &[
                "2024-01-15T10:30:00Z INFO served /users/1 in 12ms",
                "2024-01-15T10:30:05Z INFO served /users/22 in 3ms",
                "2024-01-15T10:31:00Z ERROR db timeout after 30s",
                "",
                "2024-01-15T10:32:00Z INFO served /users/333 in 7ms",
            ],
        );
        assert_eq!(s.totals.lines, 5);
        assert_eq!(s.totals.blank, 1);
        assert_eq!(s.totals.matched, 4);
        assert_eq!(s.totals.with_timestamp, 4);
        assert_eq!(s.totals.distinct_templates, 2);
        assert_eq!((s.levels.info, s.levels.error), (3, 1));

        let top = &s.templates[0];
        assert_eq!(top.template, "<ts> INFO served /users/<num> in <dur>");
        assert_eq!(top.count, 3);
        assert_eq!(top.share, 0.75);
        assert_eq!(top.level, Some(Level::Info));
        assert_eq!((top.first_line, top.last_line), (1, 5));
        assert_eq!(top.first_seen, Some(1_705_314_600));
        assert_eq!(top.last_seen, Some(1_705_314_720));
        assert_eq!(
            top.example,
            "2024-01-15T10:30:00Z INFO served /users/1 in 12ms"
        );
        assert_eq!(
            s.time_range,
            Some(TimeRange {
                first: 1_705_314_600,
                last: 1_705_314_720
            })
        );
    }

    #[test]
    fn json_lines_use_the_message_field() {
        let s = run(
            opts(),
            &[
                r#"{"time":"2024-01-15T10:30:00Z","level":"warn","msg":"slow query 120ms"}"#,
                r#"{"ts":1705314601,"level":40,"message":"slow query 95ms","rows":3}"#,
                r#"{"level":"info","status":200}"#,
                r#"{"msg":"cut off"#,
            ],
        );
        assert_eq!(s.totals.json_lines, 3);
        assert_eq!(s.totals.malformed_json, 1);
        assert_eq!(s.totals.with_timestamp, 2);
        assert_eq!(s.levels.warn, 2);
        assert_eq!(s.levels.info, 1);
        assert_eq!(s.levels.unknown, 1);
        assert_eq!(s.templates[0].template, "slow query <dur>");
        assert_eq!(s.templates[0].count, 2);
        let rest: Vec<&str> = s.templates[1..]
            .iter()
            .map(|t| t.template.as_str())
            .collect();
        assert_eq!(rest, ["{json: level,status}", r#"{<str>:"cut off"#]);
    }

    #[test]
    fn level_filter_drops_lower_and_unknown() {
        let s = run(
            Options {
                min_level: Some(Level::Warn),
                ..opts()
            },
            &["INFO a", "WARN b", "ERROR c", "no level", "FATAL d"],
        );
        assert_eq!(s.totals.matched, 3);
        assert_eq!(s.totals.filtered_out, 2);
        assert_eq!(s.levels.info, 0);
        assert_eq!(s.templates.len(), 3);
    }

    #[test]
    fn time_filter_is_half_open_and_drops_undated_lines() {
        let s = run(
            Options {
                since: Some(1_705_314_600),
                until: Some(1_705_314_660),
                ..opts()
            },
            &[
                "2024-01-15T10:29:59Z a",
                "2024-01-15T10:30:00Z b",
                "2024-01-15T10:30:59Z c",
                "2024-01-15T10:31:00Z d",
                "undated",
            ],
        );
        assert_eq!(s.totals.matched, 2);
        assert_eq!(s.totals.filtered_out, 3);
        assert_eq!(s.totals.with_timestamp, 4);
    }

    #[test]
    fn template_cap_counts_overflow() {
        let s = run(
            Options {
                max_templates: 2,
                ..opts()
            },
            &["alpha", "beta", "gamma", "alpha", "delta"],
        );
        assert_eq!(s.totals.distinct_templates, 2);
        assert_eq!(s.totals.template_overflow_lines, 2);
        assert_eq!(s.templates[0].template, "alpha");
        assert_eq!(s.templates[0].count, 2);
    }

    #[test]
    fn ties_keep_input_order_and_top_limits() {
        let s = run(
            Options { top: 2, ..opts() },
            &["one", "two", "three", "two", "one"],
        );
        let names: Vec<&str> = s.templates.iter().map(|t| t.template.as_str()).collect();
        assert_eq!(names, ["one", "two"]);
    }

    #[test]
    fn invalid_utf8_is_counted_and_kept() {
        let mut a = Analyzer::new(opts());
        a.feed(b"ERROR bad byte \xff\xfe in name 7", false);
        a.feed(b"ERROR bad byte \xff\xfe in name 8", false);
        let s = a.finish(vec![]);
        assert_eq!(s.totals.invalid_utf8, 2);
        assert_eq!(s.templates.len(), 1);
        assert_eq!(
            s.templates[0].template,
            "ERROR bad byte \u{fffd}\u{fffd} in name <num>"
        );
    }

    #[test]
    fn long_lines_are_cut_for_templates_and_examples() {
        let long = format!("INFO start {} 1", "é".repeat(2000));
        let other = format!("INFO start {} 2", "é".repeat(2000));
        let s = run(opts(), &[&long, &other]);
        assert_eq!(s.templates.len(), 1);
        assert!(s.templates[0].template.ends_with(" <truncated>"));
        assert!(s.templates[0].template.len() <= MAX_TEMPLATE_INPUT + 12);
        assert_eq!(
            s.templates[0].example.chars().count(),
            MAX_EXAMPLE_CHARS + 1
        );
    }

    #[test]
    fn empty_input() {
        let s = run(opts(), &[]);
        assert_eq!(s.totals, Totals::default());
        assert!(s.templates.is_empty() && s.bursts.is_empty());
        assert_eq!(s.histogram, None);
        assert_eq!(s.time_range, None);
    }

    #[test]
    fn bursts_are_reported() {
        let mut lines = Vec::new();
        for minute in 0..30 {
            lines.push(format!("2024-01-15T10:{minute:02}:00Z INFO tick"));
            if minute == 12 {
                for sec in 1..=20 {
                    lines.push(format!("2024-01-15T10:12:{sec:02}Z ERROR upstream down"));
                }
            }
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let s = run(
            Options {
                bucket: Some(60),
                ..opts()
            },
            &refs,
        );
        assert_eq!(s.bursts.len(), 1);
        let burst = &s.bursts[0];
        assert_eq!(
            timestamp::format_rfc3339(burst.start),
            "2024-01-15T10:12:00Z"
        );
        assert_eq!(timestamp::format_rfc3339(burst.end), "2024-01-15T10:13:00Z");
        assert_eq!((burst.errors, burst.lines), (20, 21));
        assert_eq!(s.histogram.as_ref().unwrap().buckets.len(), 30);
    }
}
