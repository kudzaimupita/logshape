//! Rendering a [`Summary`] for people (text) and for programs (JSON).

use std::io::{self, Write};

use crate::analyze::Summary;
use crate::level::Level;
use crate::timestamp::format_plain;

const BAR_WIDTH: u64 = 40;
/// Templates and examples are cut to this many characters in text output.
const TEXT_WIDTH: usize = 150;
/// Runs of empty buckets longer than this are folded into one line.
const MAX_EMPTY_RUN: i64 = 2;

/// Writes the summary as pretty-printed JSON followed by a newline.
pub fn write_json(summary: &Summary, out: &mut dyn Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, summary)?;
    out.write_all(b"\n")
}

struct Paint {
    on: bool,
}

impl Paint {
    fn wrap(&self, code: &str, text: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    fn bold(&self, text: &str) -> String {
        self.wrap("1", text)
    }

    fn dim(&self, text: &str) -> String {
        self.wrap("2", text)
    }

    fn red(&self, text: &str) -> String {
        self.wrap("31", text)
    }

    fn green(&self, text: &str) -> String {
        self.wrap("32", text)
    }

    fn level(&self, level: Option<Level>, text: &str) -> String {
        match level {
            Some(Level::Fatal | Level::Error) => self.wrap("31", text),
            Some(Level::Warn) => self.wrap("33", text),
            Some(Level::Debug | Level::Trace) | None => self.wrap("2", text),
            Some(Level::Info) => text.to_owned(),
        }
    }
}

/// `1234567` as `1,234,567`.
pub fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A byte count with a binary unit, such as `1.5 MiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A length of time in its two largest units, such as `2h 5m`.
pub fn span(secs: i64) -> String {
    const UNITS: [(i64, &str); 4] = [(86_400, "d"), (3600, "h"), (60, "m"), (1, "s")];
    let mut parts = Vec::new();
    let mut rest = secs.max(0);
    for (size, name) in UNITS {
        if rest >= size && parts.len() < 2 {
            parts.push(format!("{}{name}", rest / size));
            rest %= size;
        } else if !parts.is_empty() {
            break;
        }
    }
    if parts.is_empty() {
        "0s".to_owned()
    } else {
        parts.join(" ")
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "0.0%".to_owned()
    } else {
        format!("{:.1}%", part as f64 * 100.0 / whole as f64)
    }
}

fn bar(value: u64, max: u64) -> String {
    if value == 0 || max == 0 {
        return String::new();
    }
    let cells = (value * BAR_WIDTH).div_ceil(max).clamp(1, BAR_WIDTH);
    "█".repeat(cells as usize)
}

/// Cuts `text` to `max` characters and replaces control characters, so a
/// log line cannot send escape sequences to the terminal.
fn cut(text: &str, max: usize) -> String {
    let mut out: String = text
        .chars()
        .take(max)
        .map(|c| match c {
            '\t' => ' ',
            c if c.is_control() => '\u{fffd}',
            c => c,
        })
        .collect();
    if text.chars().nth(max).is_some() {
        out.push('…');
    }
    out
}

/// Bucket start, with only as much precision as the bucket size needs.
fn bucket_label(start: i64, size: i64) -> String {
    let full = format_plain(start);
    let keep = if size % 86_400 == 0 {
        10
    } else if size % 60 == 0 {
        16
    } else {
        19
    };
    full[..keep].to_owned()
}

/// Writes the terminal summary.
pub fn write_text(summary: &Summary, color: bool, out: &mut dyn Write) -> io::Result<()> {
    let paint = Paint { on: color };
    let t = &summary.totals;

    writeln!(
        out,
        "{}  {}",
        paint.bold("logshape"),
        summary.inputs.join(", ")
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "  {:<12} {} ({})",
        "lines",
        group(t.lines),
        bytes(t.bytes)
    )?;
    if t.filtered_out > 0 {
        writeln!(
            out,
            "  {:<12} {} ({} filtered out)",
            "matched",
            group(t.matched),
            group(t.filtered_out)
        )?;
    }
    match &summary.time_range {
        Some(range) => writeln!(
            out,
            "  {:<12} {} to {} UTC ({})",
            "time range",
            format_plain(range.first),
            format_plain(range.last),
            span(range.last - range.first)
        )?,
        None => writeln!(out, "  {:<12} no timestamps found", "time range")?,
    }
    writeln!(
        out,
        "  {:<12} {} of non-blank lines",
        "timestamps",
        percent(t.with_timestamp, t.lines - t.blank)
    )?;
    let mut notes = Vec::new();
    for (count, what) in [
        (t.json_lines, "JSON"),
        (t.malformed_json, "malformed JSON"),
        (t.invalid_utf8, "invalid UTF-8"),
        (t.truncated, "over-long"),
        (t.blank, "blank"),
    ] {
        if count > 0 {
            notes.push(format!("{} {what}", group(count)));
        }
    }
    if !notes.is_empty() {
        writeln!(out, "  {:<12} {}", "line kinds", notes.join(", "))?;
    }

    writeln!(out)?;
    writeln!(out, "{}", paint.bold("levels"))?;
    let l = &summary.levels;
    let mut rows: Vec<(Option<Level>, u64)> =
        Level::ALL.iter().map(|&lv| (Some(lv), l.get(lv))).collect();
    rows.push((None, l.unknown));
    let max = rows.iter().map(|r| r.1).max().unwrap_or(0);
    for (level, count) in rows.into_iter().filter(|r| r.1 > 0) {
        let name = level.map_or("unknown", Level::as_str);
        writeln!(
            out,
            "  {} {:>12} {:>6}  {}",
            paint.level(level, &format!("{name:<8}")),
            group(count),
            percent(count, t.matched),
            paint.level(level, &bar(count, max))
        )?;
    }
    if t.matched == 0 {
        writeln!(out, "  no lines matched")?;
    }

    writeln!(out)?;
    match &summary.histogram {
        Some(h) => {
            writeln!(
                out,
                "{}  {}",
                paint.bold("timeline"),
                paint.dim(&format!("{} buckets, UTC", span(h.bucket_seconds)))
            )?;
            let max = h.buckets.iter().map(|b| b.count).max().unwrap_or(0);
            let mut previous: Option<i64> = None;
            for b in &h.buckets {
                if let Some(prev) = previous {
                    let empty = (b.start - prev) / h.bucket_seconds - 1;
                    if empty > MAX_EMPTY_RUN {
                        let note = format!("({} empty buckets)", group(empty as u64));
                        writeln!(out, "  {}", paint.dim(&note))?;
                    } else {
                        for k in 1..=empty {
                            let start = prev + k * h.bucket_seconds;
                            writeln!(
                                out,
                                "  {}  {:<width$} {:>10}",
                                bucket_label(start, h.bucket_seconds),
                                "",
                                0,
                                width = BAR_WIDTH as usize
                            )?;
                        }
                    }
                }
                previous = Some(b.start);
                let errors = if b.errors > 0 {
                    let noun = if b.errors == 1 { "error" } else { "errors" };
                    paint.red(&format!("  {} {noun}", group(b.errors)))
                } else {
                    String::new()
                };
                writeln!(
                    out,
                    "  {}  {:<width$} {:>10}{}",
                    bucket_label(b.start, h.bucket_seconds),
                    paint.green(&bar(b.count, max)),
                    group(b.count),
                    errors,
                    // Colour codes take nine bytes that pad no column.
                    width = BAR_WIDTH as usize + if color { 9 } else { 0 }
                )?;
            }
        }
        None => {
            writeln!(out, "{}", paint.bold("timeline"))?;
            writeln!(out, "  no timestamps found")?;
        }
    }

    writeln!(out)?;
    let mut heading = format!(
        "top {} of {}",
        summary.templates.len(),
        group(t.distinct_templates)
    );
    if t.template_overflow_lines > 0 {
        heading.push_str(&format!(
            ", {} lines not grouped after the template cap",
            group(t.template_overflow_lines)
        ));
    }
    writeln!(out, "{}  {}", paint.bold("templates"), paint.dim(&heading))?;
    for (rank, tpl) in summary.templates.iter().enumerate() {
        let level = tpl.level.map_or("-", Level::as_str);
        writeln!(
            out,
            "  {:>2}. {:>10} {:>6}  {}  {}",
            rank + 1,
            group(tpl.count),
            percent(tpl.count, t.matched),
            paint.level(tpl.level, &format!("{level:<5}")),
            cut(&tpl.template, TEXT_WIDTH)
        )?;
        let seen = match (tpl.first_seen, tpl.last_seen) {
            (Some(first), Some(last)) => {
                format!("{} to {}", format_plain(first), format_plain(last))
            }
            _ => format!(
                "lines {} to {}",
                group(tpl.first_line),
                group(tpl.last_line)
            ),
        };
        let indent = " ".repeat(31);
        writeln!(out, "{indent}{}", paint.dim(&format!("seen  {seen}")))?;
        writeln!(
            out,
            "{indent}{}",
            paint.dim(&format!("e.g.  {}", cut(&tpl.example, TEXT_WIDTH)))
        )?;
    }
    if summary.templates.is_empty() {
        writeln!(out, "  none")?;
    }

    writeln!(out)?;
    writeln!(out, "{}", paint.bold("error bursts"))?;
    let size = summary.histogram.as_ref().map_or(1, |h| h.bucket_seconds);
    for b in &summary.bursts {
        writeln!(
            out,
            "  {} to {}  {}",
            bucket_label(b.start, size),
            bucket_label(b.end, size),
            paint.red(&format!(
                "{} errors in {} lines, peak {} per bucket against a median of {}",
                group(b.errors),
                group(b.lines),
                group(b.peak_errors),
                group(b.baseline_errors)
            ))
        )?;
    }
    if summary.bursts.is_empty() {
        writeln!(out, "  none")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{Analyzer, Options};

    #[test]
    fn number_formatting() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(1_234_567), "1,234,567");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(percent(1, 8), "12.5%");
        assert_eq!(percent(1, 0), "0.0%");
    }

    #[test]
    fn spans() {
        assert_eq!(span(0), "0s");
        assert_eq!(span(45), "45s");
        assert_eq!(span(60), "1m");
        assert_eq!(span(7500), "2h 5m");
        assert_eq!(span(90_061), "1d 1h");
        assert_eq!(span(3601), "1h");
        assert_eq!(span(604_800), "7d");
    }

    #[test]
    fn bars_scale_and_never_vanish() {
        assert_eq!(bar(0, 10), "");
        assert_eq!(bar(10, 10).chars().count(), BAR_WIDTH as usize);
        assert_eq!(bar(1, 1_000_000).chars().count(), 1);
        assert_eq!(bar(5, 10).chars().count(), BAR_WIDTH as usize / 2);
    }

    #[test]
    fn cut_shortens_and_strips_control_characters() {
        assert_eq!(cut("abcdef", 3), "abc…");
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("a\tb\x1b[31mc\x00", 20), "a b\u{fffd}[31mc\u{fffd}");
    }

    #[test]
    fn bucket_labels_follow_bucket_size() {
        let ts = 1_705_314_600;
        assert_eq!(bucket_label(ts, 1), "2024-01-15 10:30:00");
        assert_eq!(bucket_label(ts, 300), "2024-01-15 10:30");
        assert_eq!(bucket_label(ts, 86_400), "2024-01-15");
    }

    fn sample() -> Summary {
        let mut a = Analyzer::new(Options {
            year: 2024,
            bucket: Some(60),
            ..Options::default()
        });
        for line in [
            "2024-01-15T10:30:00Z INFO started worker 1",
            "2024-01-15T10:30:20Z INFO started worker 2",
            "2024-01-15T10:40:00Z ERROR lost worker 2",
            "stray line",
        ] {
            a.feed(line.as_bytes(), false);
        }
        a.finish(vec!["sample.log".into()])
    }

    #[test]
    fn text_report_has_every_section_and_no_colour_codes() {
        let mut out = Vec::new();
        write_text(&sample(), false, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains('\x1b'));
        for needle in [
            "logshape  sample.log",
            "time range   2024-01-15 10:30:00 to 2024-01-15 10:40:00 UTC (10m)",
            "timestamps   75.0% of non-blank lines",
            "levels",
            "timeline  1m buckets, UTC",
            "(9 empty buckets)",
            "templates  top 3 of 3",
            "<ts> INFO started worker <num>",
            "seen  2024-01-15 10:30:00 to 2024-01-15 10:30:20",
            "seen  lines 4 to 4",
            "e.g.  stray line",
            "error bursts\n  none",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn colour_is_opt_in() {
        let mut out = Vec::new();
        write_text(&sample(), true, &mut out).unwrap();
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("\x1b[1mlogshape\x1b[0m"));
    }

    #[test]
    fn json_report_is_valid_json() {
        let mut out = Vec::new();
        write_json(&sample(), &mut out).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["time_range"]["first"], "2024-01-15T10:30:00Z");
        assert_eq!(value["templates"][0]["level"], "info");
        assert_eq!(value["templates"][2]["level"], serde_json::Value::Null);
        assert_eq!(value["histogram"]["bucket_seconds"], 60);
    }
}
