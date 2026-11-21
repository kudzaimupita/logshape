use std::io::{self, BufWriter, IsTerminal, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};

use logshape::analyze::{Analyzer, Options};
use logshape::input::{self, LineReader};
use logshape::level::Level;
use logshape::{report, timestamp};

/// Show the shape of a log: line templates, levels, a time histogram and
/// error bursts, in one streaming pass.
#[derive(Debug, Parser)]
#[command(version, about, max_term_width = 100)]
struct Cli {
    /// Log files to read; `-` or no file reads stdin. Gzip is detected.
    files: Vec<String>,

    /// Number of templates to show.
    #[arg(long, value_name = "N", default_value_t = 10)]
    top: usize,

    /// Only count lines at this level or above (trace, debug, info, warn,
    /// error, fatal). Lines without a level are dropped.
    #[arg(long, value_name = "LEVEL")]
    level: Option<Level>,

    /// Only count lines at or after this time, e.g. 2024-01-15T10:00:00Z or
    /// 2024-01-15. Lines without a timestamp are dropped.
    #[arg(long, value_name = "TIME")]
    since: Option<String>,

    /// Only count lines before this time.
    #[arg(long, value_name = "TIME")]
    until: Option<String>,

    /// Histogram bucket size, e.g. 30s, 5m, 1h, 1d [default: automatic].
    #[arg(long, value_name = "DURATION", value_parser = parse_bucket)]
    bucket: Option<i64>,

    /// Print the summary as JSON.
    #[arg(long)]
    json: bool,

    /// Never colour the output. Colour is also off when stdout is not a
    /// terminal or NO_COLOR is set.
    #[arg(long)]
    no_color: bool,

    /// Year for timestamps that do not carry one, such as syslog
    /// [default: the current year].
    #[arg(long, value_name = "YEAR", value_parser = clap::value_parser!(i32).range(1970..=9999))]
    year: Option<i32>,

    /// Upper bound on distinct templates kept in memory.
    #[arg(long, value_name = "N", default_value_t = 10_000)]
    max_templates: usize,
}

fn parse_bucket(text: &str) -> Result<i64, String> {
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let scale = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err("expected a whole number with s, m, h or d, such as 5m".into()),
    };
    match digits.parse::<i64>() {
        Ok(n) if n > 0 && n <= 366 * 86_400 / scale => Ok(n * scale),
        _ => Err("expected a duration between 1s and 366d, such as 5m".into()),
    }
}

/// Parses `--since` or `--until`, leaving through clap's usage error (exit
/// code 2) when the value is not a time.
fn parse_time(flag: &str, text: Option<&str>, year: i32) -> Option<i64> {
    let text = text?;
    let parsed = timestamp::parse_whole(text, year);
    if parsed.is_none() {
        Cli::command()
            .error(
                ErrorKind::ValueValidation,
                format!(
                    "invalid value '{text}' for '{flag} <TIME>': \
                     expected a time such as 2024-01-15T10:00:00Z or 2024-01-15"
                ),
            )
            .exit();
    }
    parsed
}

fn run(cli: Cli) -> Result<(), String> {
    let year = cli.year.unwrap_or_else(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        timestamp::year_of(now)
    });
    let since = parse_time("--since", cli.since.as_deref(), year);
    let until = parse_time("--until", cli.until.as_deref(), year);

    let mut analyzer = Analyzer::new(Options {
        top: cli.top,
        min_level: cli.level,
        since,
        until,
        bucket: cli.bucket,
        year,
        max_templates: cli.max_templates,
    });

    let files = if cli.files.is_empty() {
        vec!["-".to_owned()]
    } else {
        cli.files
    };
    let mut line = Vec::with_capacity(4096);
    for path in &files {
        let name = if path == "-" { "stdin" } else { path };
        let reader = input::open(path).map_err(|e| format!("{name}: {e}"))?;
        let mut reader = LineReader::new(reader);
        while let Some(info) = reader
            .read_line(&mut line)
            .map_err(|e| format!("{name}: {e}"))?
        {
            analyzer.add_bytes(info.bytes);
            analyzer.feed(&line, info.truncated);
        }
    }
    let inputs = files
        .into_iter()
        .map(|p| if p == "-" { "stdin".to_owned() } else { p })
        .collect();
    let summary = analyzer.finish(inputs);

    let stdout = io::stdout();
    let color = !cli.json
        && !cli.no_color
        && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        && stdout.is_terminal();
    let mut out = BufWriter::new(stdout.lock());
    let written = if cli.json {
        report::write_json(&summary, &mut out)
    } else {
        report::write_text(&summary, color, &mut out)
    }
    .and_then(|()| out.flush());
    match written {
        // A closed pipe (`logshape app.log | head`) is not a failure.
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(format!("stdout: {e}")),
        _ => Ok(()),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("logshape: {message}");
            ExitCode::FAILURE
        }
    }
}
