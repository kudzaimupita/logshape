//! End-to-end tests: fixture logs through the compiled binary.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::Value;

fn logshape() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_logshape"));
    cmd.env_remove("NO_COLOR");
    cmd
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn scratch(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// Runs with `--json` plus `args` and parses stdout.
fn json(args: &[&str]) -> Value {
    let out = logshape().arg("--json").args(args).assert().success();
    serde_json::from_slice(&out.get_output().stdout).expect("stdout is JSON")
}

fn text(args: &[&str]) -> String {
    let out = logshape().args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).expect("stdout is UTF-8")
}

fn templates(v: &Value) -> Vec<&str> {
    v["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["template"].as_str().unwrap())
        .collect()
}

fn keys(v: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

#[test]
fn app_log_text_summary() {
    let out = text(&[&fixture("app.log")]);
    assert!(
        !out.contains('\x1b'),
        "no colour when stdout is not a terminal"
    );
    for needle in [
        "lines        124 (9.5 KiB)",
        "time range   2024-03-01 09:00:02 to 2024-03-01 09:59:02 UTC (59m)",
        "timestamps   98.4% of non-blank lines",
        "timeline  5m buckets, UTC",
        "templates  top 8 of 8",
        "<ts> INFO  [http] GET /api/users/<num> completed in <dur> status=<num>",
        "<ts> ERROR [db] connection to <ip> refused (attempt <num>)",
        "<ts> ERROR [worker] job <uuid> failed: timeout",
        "e.g.  2024-03-01T09:00:20Z DEBUG [cache] evicted 4 entries, 512MB in use",
        "2024-03-01 09:40 to 2024-03-01 09:45  14 errors in 23 lines",
    ] {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }
}

#[test]
fn app_log_json_summary() {
    let v = json(&[&fixture("app.log")]);
    assert_eq!(v["totals"]["lines"], 124);
    assert_eq!(v["totals"]["matched"], 124);
    assert_eq!(v["totals"]["with_timestamp"], 122);
    assert_eq!(
        v["levels"],
        serde_json::json!({
            "fatal": 0, "error": 15, "warn": 5, "info": 90, "debug": 12, "trace": 0, "unknown": 2
        })
    );

    let top = &v["templates"][0];
    assert_eq!(top["count"], 60);
    assert_eq!(top["level"], "info");
    assert_eq!(top["first_seen"], "2024-03-01T09:00:02Z");
    assert_eq!(top["last_seen"], "2024-03-01T09:59:02Z");
    assert_eq!(
        (top["first_line"].as_u64(), top["last_line"].as_u64()),
        (Some(1), Some(124))
    );
    let share = top["share"].as_f64().unwrap();
    assert!((share - 60.0 / 124.0).abs() < 1e-12);

    let h = &v["histogram"];
    assert_eq!(h["bucket_seconds"], 300);
    let buckets = h["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 12);
    let counted: u64 = buckets.iter().map(|b| b["count"].as_u64().unwrap()).sum();
    assert_eq!(counted, 122);

    assert_eq!(
        v["bursts"],
        serde_json::json!([{
            "start": "2024-03-01T09:40:00Z",
            "end": "2024-03-01T09:45:00Z",
            "errors": 14,
            "lines": 23,
            "peak_errors": 14,
            "baseline_errors": 0
        }])
    );
}

#[test]
fn json_output_schema() {
    let v = json(&[&fixture("app.log")]);
    assert_eq!(
        keys(&v),
        [
            "bursts",
            "histogram",
            "inputs",
            "levels",
            "templates",
            "time_range",
            "totals",
            "version"
        ]
    );
    assert_eq!(v["version"], 1);
    assert!(v["inputs"].as_array().unwrap().iter().all(Value::is_string));
    assert_eq!(
        keys(&v["totals"]),
        [
            "blank",
            "bytes",
            "distinct_templates",
            "filtered_out",
            "invalid_utf8",
            "json_lines",
            "lines",
            "malformed_json",
            "matched",
            "template_overflow_lines",
            "truncated",
            "with_timestamp"
        ]
    );
    assert!(v["totals"].as_object().unwrap().values().all(Value::is_u64));
    assert_eq!(
        keys(&v["levels"]),
        ["debug", "error", "fatal", "info", "trace", "unknown", "warn"]
    );
    assert!(v["levels"].as_object().unwrap().values().all(Value::is_u64));
    assert_eq!(keys(&v["time_range"]), ["first", "last"]);
    assert_eq!(keys(&v["histogram"]), ["bucket_seconds", "buckets"]);
    for bucket in v["histogram"]["buckets"].as_array().unwrap() {
        assert_eq!(keys(bucket), ["count", "errors", "start"]);
        assert!(is_rfc3339(&bucket["start"]));
        assert!(bucket["count"].is_u64() && bucket["errors"].is_u64());
    }
    let templates = v["templates"].as_array().unwrap();
    assert!(!templates.is_empty());
    for t in templates {
        assert_eq!(
            keys(t),
            [
                "count",
                "example",
                "first_line",
                "first_seen",
                "last_line",
                "last_seen",
                "level",
                "share",
                "template"
            ]
        );
        assert!(t["template"].is_string() && t["example"].is_string());
        assert!(t["count"].is_u64() && t["first_line"].is_u64() && t["last_line"].is_u64());
        assert!((0.0..=1.0).contains(&t["share"].as_f64().unwrap()));
        assert!(t["level"].is_null() || t["level"].is_string());
        assert!(t["first_seen"].is_null() || is_rfc3339(&t["first_seen"]));
        assert!(t["last_seen"].is_null() || is_rfc3339(&t["last_seen"]));
    }
    let bursts = v["bursts"].as_array().unwrap();
    assert!(!bursts.is_empty());
    for b in bursts {
        assert_eq!(
            keys(b),
            [
                "baseline_errors",
                "end",
                "errors",
                "lines",
                "peak_errors",
                "start"
            ]
        );
        assert!(is_rfc3339(&b["start"]) && is_rfc3339(&b["end"]));
    }
}

fn is_rfc3339(v: &Value) -> bool {
    let Some(s) = v.as_str() else { return false };
    let shape = "dddd-dd-ddTdd:dd:ddZ";
    s.len() == shape.len()
        && s.chars().zip(shape.chars()).all(|(c, want)| match want {
            'd' => c.is_ascii_digit(),
            other => c == other,
        })
}

#[test]
fn json_schema_when_nothing_has_a_timestamp() {
    let out = logshape()
        .arg("--json")
        .write_stdin("plain line\nanother 7\n")
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert!(v["time_range"].is_null());
    assert!(v["histogram"].is_null());
    assert_eq!(v["bursts"], serde_json::json!([]));
    assert_eq!(v["inputs"], serde_json::json!(["stdin"]));
    assert!(v["templates"][0]["first_seen"].is_null());
    assert!(v["templates"][0]["level"].is_null());
}

#[test]
fn access_log_keeps_method_route_and_status() {
    let v = json(&["--top", "50", &fixture("access.log")]);
    assert_eq!(v["totals"]["lines"], 40);
    assert_eq!(v["totals"]["with_timestamp"], 40);
    // The +0100 offset is applied.
    assert_eq!(v["time_range"]["first"], "2024-03-02T13:00:00Z");
    assert_eq!(v["levels"]["unknown"], 40);
    let all = templates(&v);
    for want in [
        r#"<ip> - - [<ts>] "GET /users/<num> HTTP/<num>" 200 <num> <str> <str>"#,
        r#"<ip> - - [<ts>] "GET /users/<num>/orders HTTP/<num>" 404 <num> <str> <str>"#,
        r#"<ip> - - [<ts>] "GET /search?<query> HTTP/<num>" 502 <num> <str> <str>"#,
    ] {
        assert!(all.contains(&want), "missing {want} in {all:#?}");
    }
}

#[test]
fn syslog_takes_its_year_from_the_flag() {
    let v = json(&["--year", "2024", &fixture("syslog.log")]);
    assert_eq!(v["time_range"]["first"], "2024-03-03T06:25:01Z");
    assert_eq!(v["time_range"]["last"], "2024-03-12T23:59:59Z");
    assert_eq!(
        templates(&v)[0],
        "<ts> web1 sshd[<num>]: Accepted publickey for deploy from <ip> port <num> ssh2"
    );
    assert_eq!(v["templates"][0]["count"], 2);

    let v = json(&["--year", "2021", &fixture("syslog.log")]);
    assert_eq!(v["time_range"]["first"], "2021-03-03T06:25:01Z");
}

#[test]
fn json_lines_with_malformed_entries() {
    let v = json(&["--top", "20", &fixture("events.jsonl")]);
    let t = &v["totals"];
    assert_eq!(t["lines"], 12);
    assert_eq!(t["json_lines"], 9);
    assert_eq!(t["malformed_json"], 2);
    assert_eq!(t["with_timestamp"], 9);
    assert_eq!(
        v["levels"],
        serde_json::json!({
            "fatal": 0, "error": 2, "warn": 2, "info": 4, "debug": 1, "trace": 0, "unknown": 3
        })
    );
    assert_eq!(v["time_range"]["first"], "2024-03-04T12:00:00Z");
    assert_eq!(v["time_range"]["last"], "2024-03-04T12:00:09Z");
    let all = templates(&v);
    assert_eq!(all[0], "request done");
    assert_eq!(all[1], "retrying upload <num> of <num> after <dur>");
    for want in [
        "upload failed for user <hex>",
        "gc pause <dur>",
        "{json: time,level,status,route}",
        "{broken json here}",
        "[<num>,<num>,<num>]",
        "odd fields",
    ] {
        assert!(all.contains(&want), "missing {want} in {all:#?}");
    }
}

#[test]
fn messy_input_is_counted_not_fatal() {
    let v = json(&["--top", "50", &fixture("messy.log")]);
    let t = &v["totals"];
    assert_eq!(t["lines"], 14);
    assert_eq!(t["blank"], 2);
    assert_eq!(t["matched"], 12);
    assert_eq!(t["invalid_utf8"], 4);
    assert_eq!(t["malformed_json"], 1);
    assert_eq!(t["with_timestamp"], 5);
    assert_eq!(v["levels"]["error"], 3);
    assert_eq!(v["levels"]["unknown"], 5);

    // The two lines with invalid bytes still share one template, with the
    // bad bytes shown as U+FFFD.
    let top = &v["templates"][0];
    assert_eq!(
        top["template"],
        "<ts> ERROR caf\u{fffd} name \u{fffd}\u{fffd} rejected <num>"
    );
    assert_eq!(top["count"], 2);

    let all = templates(&v);
    // An impossible date is not a timestamp, and the CRLF ending is removed.
    assert!(all.contains(&"<num>-<num>-<num> <num>:<num>:<num> INFO impossible date"));
    assert!(all.iter().all(|t| !t.ends_with('\r')));
}

#[test]
fn messy_input_text_has_no_control_characters() {
    let out = text(&[&fixture("messy.log")]);
    assert!(out.contains("4 invalid UTF-8"));
    assert!(out.contains("1 malformed JSON"));
    assert!(out.contains("2 blank"));
    assert!(out.chars().all(|c| c == '\n' || !c.is_control()), "{out:?}");
}

#[test]
fn escape_sequences_in_logs_do_not_reach_the_terminal() {
    let out = logshape()
        .write_stdin("INFO \x1b[2J\x1b[31mcleared\x07\n")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains('\x1b') && !stdout.contains('\x07'));
}

#[test]
fn stdin_and_dash() {
    let input = fs::read(fixture("app.log")).unwrap();
    for args in [&[][..], &["-"][..]] {
        let out = logshape()
            .arg("--json")
            .args(args)
            .write_stdin(input.clone())
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v["totals"]["lines"], 124);
        assert_eq!(v["inputs"], serde_json::json!(["stdin"]));
    }
}

#[test]
fn several_files_are_one_stream() {
    let app = fixture("app.log");
    let events = fixture("events.jsonl");
    let v = json(&[&app, &events]);
    assert_eq!(v["totals"]["lines"], 136);
    assert_eq!(v["inputs"], serde_json::json!([app, events]));
    assert_eq!(v["time_range"]["first"], "2024-03-01T09:00:02Z");
    assert_eq!(v["time_range"]["last"], "2024-03-04T12:00:09Z");
    // Line numbers continue across files.
    let last_line = v["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["last_line"].as_u64().unwrap())
        .max();
    assert!(last_line > Some(124));
}

#[test]
fn gzip_is_detected_by_content() {
    let plain = fs::read(fixture("app.log")).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&plain).unwrap();
    let packed = encoder.finish().unwrap();
    // No .gz extension: detection goes by the magic bytes.
    let path = scratch("app-log-packed");
    fs::write(&path, &packed).unwrap();

    let v = json(&[path.to_str().unwrap()]);
    assert_eq!(v["totals"]["lines"], 124);
    assert_eq!(v["totals"]["bytes"], plain.len());
    assert_eq!(v["levels"]["error"], 15);

    let out = logshape()
        .arg("--json")
        .write_stdin(packed)
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["totals"]["lines"], 124);
}

#[test]
fn level_filter() {
    let v = json(&["--level", "warn", &fixture("app.log")]);
    assert_eq!(v["totals"]["matched"], 20);
    assert_eq!(v["totals"]["filtered_out"], 104);
    assert_eq!(v["levels"]["info"], 0);
    assert_eq!(v["levels"]["unknown"], 0);
    assert_eq!(v["levels"]["error"], 15);
    assert_eq!(
        templates(&v)[0],
        "<ts> ERROR [db] connection to <ip> refused (attempt <num>)"
    );

    let v = json(&["--level", "ERROR", &fixture("app.log")]);
    assert_eq!(v["totals"]["matched"], 15);
}

#[test]
fn time_window_filter() {
    let v = json(&[
        "--since",
        "2024-03-01T09:40:00Z",
        "--until",
        "2024-03-01 09:42:00",
        &fixture("app.log"),
    ]);
    // 2 + 1 request lines, 1 cache line, 14 errors; the two stack trace
    // lines have no timestamp and are dropped.
    assert_eq!(v["totals"]["matched"], 18);
    assert_eq!(v["levels"]["error"], 14);
    assert_eq!(v["time_range"]["first"], "2024-03-01T09:40:02Z");
    assert_eq!(v["time_range"]["last"], "2024-03-01T09:41:50Z");

    let v = json(&["--since", "2024-03-02", &fixture("app.log")]);
    assert_eq!(v["totals"]["matched"], 0);
    assert_eq!(v["totals"]["filtered_out"], 124);
    assert_eq!(v["templates"], serde_json::json!([]));
}

#[test]
fn top_limits_templates() {
    let v = json(&["--top", "2", &fixture("app.log")]);
    assert_eq!(v["templates"].as_array().unwrap().len(), 2);
    assert_eq!(v["totals"]["distinct_templates"], 8);
    let out = text(&["--top", "2", &fixture("app.log")]);
    assert!(out.contains("templates  top 2 of 8"));
}

#[test]
fn explicit_bucket() {
    let v = json(&["--bucket", "30m", &fixture("app.log")]);
    assert_eq!(v["histogram"]["bucket_seconds"], 1800);
    assert_eq!(
        v["histogram"]["buckets"],
        serde_json::json!([
            {"start": "2024-03-01T09:00:00Z", "count": 53, "errors": 0},
            {"start": "2024-03-01T09:30:00Z", "count": 69, "errors": 15}
        ])
    );
    let out = text(&["--bucket", "1m", &fixture("app.log")]);
    assert!(out.contains("timeline  1m buckets, UTC"));
    assert!(out.contains("2024-03-01 09:40 to 2024-03-01 09:42  14 errors in 18 lines, peak 8"));
}

#[test]
fn template_cap_is_reported() {
    let v = json(&["--max-templates", "3", &fixture("app.log")]);
    assert_eq!(v["totals"]["distinct_templates"], 3);
    let overflow = v["totals"]["template_overflow_lines"].as_u64().unwrap();
    let kept: u64 = v["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["count"].as_u64().unwrap())
        .sum();
    assert_eq!(kept + overflow, 124);
    let out = text(&["--max-templates", "3", &fixture("app.log")]);
    assert!(out.contains("lines not grouped after the template cap"));
}

#[test]
fn over_long_lines_are_cut_not_buffered() {
    let mut input = vec![b'x'; 3 << 20];
    input.extend_from_slice(b"\n2024-03-01T09:00:00Z INFO after the long line\n");
    let out = logshape()
        .arg("--json")
        .write_stdin(input)
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["totals"]["lines"], 2);
    assert_eq!(v["totals"]["truncated"], 1);
    assert_eq!(v["totals"]["bytes"], (3 << 20) + 47);
    assert_eq!(v["levels"]["info"], 1);
}

#[test]
fn empty_input() {
    let out = logshape().write_stdin("").assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("lines        0 (0 B)"));
    assert!(stdout.contains("no lines matched"));
    assert!(stdout.contains("no timestamps found"));
}

#[test]
fn colour_flags_are_accepted() {
    let plain = text(&["--no-color", &fixture("app.log")]);
    assert!(!plain.contains('\x1b'));
    let out = logshape()
        .env("NO_COLOR", "1")
        .arg(fixture("app.log"))
        .assert()
        .success();
    assert!(!out.get_output().stdout.contains(&0x1b));
}

#[test]
fn missing_file_fails_with_a_message() {
    let out = logshape()
        .arg("no/such/file.log")
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.starts_with("logshape: no/such/file.log: "),
        "{stderr}"
    );
    assert!(out.get_output().stdout.is_empty());
}

#[test]
fn bad_arguments_are_rejected() {
    for args in [
        &["--bucket", "soon"][..],
        &["--bucket", "0s"],
        &["--bucket", "5w"],
        &["--level", "loud"],
        &["--top", "-1"],
        &["--year", "12"],
        &["--frobnicate"],
    ] {
        logshape()
            .args(args)
            .write_stdin("")
            .assert()
            .failure()
            .code(2);
    }
    for flag in ["--since", "--until"] {
        let out = logshape()
            .args([flag, "last tuesday"])
            .write_stdin("")
            .assert()
            .failure()
            .code(2);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(
            stderr.contains(flag) && stderr.contains("last tuesday"),
            "{stderr}"
        );
    }
}

#[test]
fn help_and_version() {
    let help = text(&["--help"]);
    for flag in [
        "--top",
        "--level",
        "--since",
        "--until",
        "--bucket",
        "--json",
        "--no-color",
    ] {
        assert!(help.contains(flag), "{flag} missing from help");
    }
    let version = text(&["--version"]);
    assert_eq!(
        version.trim(),
        concat!("logshape ", env!("CARGO_PKG_VERSION"))
    );
}
