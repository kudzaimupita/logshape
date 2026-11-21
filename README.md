# logshape

`logshape` tells you the shape of a log file: what kinds of lines it
contains, how often each kind appears, and when. It reads the input once,
line by line, and its memory use does not grow with the size of the file.

It is the first thing to run on a log you have not seen before, or on one
that is too big to open. Instead of paging through gigabytes you get a
summary like this:

```console
$ logshape --top 3 tests/fixtures/app.log
logshape  tests/fixtures/app.log

  lines        124 (9.5 KiB)
  time range   2024-03-01 09:00:02 to 2024-03-01 09:59:02 UTC (59m)
  timestamps   98.4% of non-blank lines

levels
  error              15  12.1%  ███████
  warn                5   4.0%  ███
  info               90  72.6%  ████████████████████████████████████████
  debug              12   9.7%  ██████
  unknown             2   1.6%  █

timeline  5m buckets, UTC
  2024-03-01 09:00  ████████████████                                  9
  2024-03-01 09:05  ████████████████                                  9
  2024-03-01 09:10  ████████████████                                  9
  2024-03-01 09:15  ████████████████                                  9
  2024-03-01 09:20  ████████████████                                  9
  2024-03-01 09:25  ██████████████                                    8
  2024-03-01 09:30  ██████████████████                               10
  2024-03-01 09:35  ██████████████                                    8
  2024-03-01 09:40  ████████████████████████████████████████         23  14 errors
  2024-03-01 09:45  ████████████████                                  9
  2024-03-01 09:50  ██████████████████                               10  1 error
  2024-03-01 09:55  ████████████████                                  9

templates  top 3 of 8
   1.         60  48.4%  info   <ts> INFO  [http] GET /api/users/<num> completed in <dur> status=<num>
                               seen  2024-03-01 09:00:02 to 2024-03-01 09:59:02
                               e.g.  2024-03-01T09:00:02Z INFO  [http] GET /api/users/17 completed in 8ms status=200
   2.         30  24.2%  info   <ts> INFO  [http] POST /api/orders completed in <dur> status=<num>
                               seen  2024-03-01 09:00:11 to 2024-03-01 09:58:11
                               e.g.  2024-03-01T09:00:11Z INFO  [http] POST /api/orders completed in 20ms status=201
   3.         14  11.3%  error  <ts> ERROR [db] connection to <ip> refused (attempt <num>)
                               seen  2024-03-01 09:40:35 to 2024-03-01 09:41:50
                               e.g.  2024-03-01T09:40:35Z ERROR [db] connection to 10.2.0.7:5432 refused (attempt 1)

error bursts
  2024-03-01 09:40 to 2024-03-01 09:45  14 errors in 23 lines, peak 14 per bucket against a median of 0
```

## Quick start

You need a recent stable Rust toolchain. From a clone of this repository:

```sh
cargo test                         # unit and end-to-end tests
cargo run --release -- tests/fixtures/app.log
cargo install --path .             # puts `logshape` on your PATH
```

## Usage

```text
logshape [OPTIONS] [FILES]...
```

With no file, or with `-`, it reads stdin. Several files are read as one
stream. Gzip input is detected by its first two bytes, so it works for files
and for pipes whatever the file is called.

| Flag                   | Meaning                                                                 |
|------------------------|-------------------------------------------------------------------------|
| `--top <N>`            | Number of templates to show. Default 10.                                |
| `--level <LEVEL>`      | Only count lines at this level or above. Lines with no level are dropped. |
| `--since <TIME>`       | Only count lines at or after this time. Lines with no timestamp are dropped. |
| `--until <TIME>`       | Only count lines before this time.                                      |
| `--bucket <DURATION>`  | Histogram bucket, such as `30s`, `5m`, `1h`, `1d`. Default: automatic.  |
| `--json`               | Print the summary as JSON.                                              |
| `--no-color`           | No colour. Colour is also off when stdout is not a terminal or `NO_COLOR` is set. |
| `--year <YEAR>`        | Year for timestamps that have none (syslog). Default: the current year. |
| `--max-templates <N>`  | Cap on distinct templates held in memory. Default 10000.                |

`--since` and `--until` take any timestamp form the tool detects, or a bare
date such as `2024-03-01` (midnight UTC).

```sh
logshape app.log                                   # the whole file
zcat -f /var/log/syslog* | logshape --year 2024    # rotated syslog
logshape --level warn --since 2024-03-01T09:30:00Z app.log
logshape --json app.log | jq '.bursts'
```

Exit codes: 0 on success, 1 when an input cannot be read, 2 for bad
arguments.

## How it works

Each line goes through four small steps. The rules for each step are
documented at the top of the source file named.

**Timestamp** (`src/timestamp.rs`). The first 64 bytes of the line are
searched for one of these forms:

| Form                   | Example                            |
|------------------------|------------------------------------|
| ISO 8601 / RFC 3339    | `2024-01-15T10:30:00.123+02:00`    |
| ISO-like with a space  | `2024-01-15 10:30:00,123`          |
| Slash dates (nginx)    | `2024/01/15 10:30:00`              |
| Common Log Format      | `15/Jan/2024:10:30:00 +0000`       |
| Syslog (RFC 3164)      | `Jan  5 10:30:00`                  |
| ctime (Apache errors)  | `Mon Jan 15 10:30:00.123456 2024`  |

Offsets are applied; a timestamp without a zone is read as UTC. Everything
is reported in UTC with one-second resolution. The date arithmetic is done
in the crate, with no time library.

**Level** (`src/level.rs`). Six levels are reported: fatal, error, warn,
info, debug and trace. Common spellings fold into them (`WARNING`, `ERR`,
`CRIT`, `NOTICE` and so on). An upper-case level word counts wherever it
appears near the start of the line. A lower-case word counts only when the
line marks it as a level, as in `[error]`, `error:` or `level=error`, so
"no error found" is not an error line.

**JSON lines** (`src/json.rs`). A line that starts with `{` is parsed as
JSON. The timestamp, level and message come from the usual keys (`time`,
`ts`, `@timestamp`, `level`, `severity`, `msg`, `message` and a few more);
numeric epochs and pino-style numeric levels are understood. The other
fields are skipped by the parser without being built into values. A line
that starts with `{` but does not parse is counted as malformed and handled
as plain text.

**Template** (`src/mask.rs`). The variable parts of the line are replaced by
placeholders, so lines of the same kind become the same string:

| Placeholder | Matches                                                           |
|-------------|-------------------------------------------------------------------|
| `<ts>`      | the line timestamp, and any other ISO 8601 timestamp              |
| `<str>`     | `"double quoted"` and `'single quoted'` strings                   |
| `<uuid>`    | `8-4-4-4-12` hex UUIDs                                            |
| `<ip>`      | IPv4 with optional `:port` or `/prefix`; IPv6 with `::` or 8 groups |
| `<hex>`     | `0x…`, or 8+ hex characters with at least one digit and one letter |
| `<id>`      | other tokens of 8+ characters with letters and 3+ digits          |
| `<dur>`     | a number with a time unit: `ns us µs ms s sec m min h`            |
| `<num>`     | integers and decimals, also before a short unit (`<num>MB`)       |

A token is a run of letters, digits and `_`. Since `/` separates tokens,
paths with ids need no rule of their own: `/users/42/orders/9f8e7d6c`
becomes `/users/<num>/orders/<hex>`. Digits inside a short word are left
alone, so `http2`, `sha256` and `x86_64` stay readable.

There is one exception, for access logs. A quoted HTTP request line is not
reduced to `<str>`: its inside is masked like other text, the query string
becomes `?<query>`, and the status code after it is kept. So

```text
203.0.113.7 - - [02/Mar/2024:14:01:07 +0100] "GET /users/20?full=1 HTTP/1.1" 200 317 "-" "curl/8.1"
<ip> - - [<ts>] "GET /users/<num>?<query> HTTP/<num>" 200 <num> <str> <str>
```

The masker is a hand-written scanner over bytes with no regular
expressions. For a JSON line the template is the masked message; a JSON line
with no message gets a template made of its key names.

### Memory

Nothing is kept per line. What is kept is bounded:

- **Lines.** A line is cut at 1 MiB and the rest is read and thrown away, so
  a file with no newlines cannot fill memory. Only the first 1024 bytes of a
  line go into its template.
- **Templates.** A hash map from template to count, first and last time,
  and one example line (the first one seen, cut to 200 characters). The map
  stops growing at `--max-templates`; lines that would add a template after
  that are counted and reported, not grouped.
- **Histogram.** Counting starts in one-second buckets. When the covered
  span needs more than 4096 buckets, neighbours are merged into the next
  size on a fixed ladder (1s, 5s, 10s, 30s, 1m, 5m, … 1d, 7d). Every size is
  a multiple of the one before it and buckets are aligned to the epoch, so
  merging loses nothing: the result equals what the final size would have
  produced from the start. A test checks exactly that. The report uses the
  smallest size that fits in 40 buckets.

The template map uses the standard library's SipHash rather than a faster
hash. Log content is often written by other people, and a predictable hash
would let them slow the tool down.

### Bursts

A burst is one or more adjacent histogram buckets whose error count (error
and fatal lines) is at least 5 and at least 3 times the median error count
per bucket, where empty buckets count as zero. A log with a steady stream
of errors has no bursts; one with a sudden cluster has.

### Terminal safety

Control characters in templates and examples are replaced before printing,
so a log line cannot send escape sequences to your terminal. The JSON output
escapes them as JSON does.

## JSON output

`--json` prints one object. Times are RFC 3339 strings in UTC. `time_range`
and `histogram` are `null` when no line has a timestamp. `histogram.buckets`
lists only non-empty buckets.

```json
{
  "version": 1,
  "inputs": ["app.log"],
  "totals": {
    "lines": 124, "bytes": 9713, "blank": 0, "matched": 124, "filtered_out": 0,
    "with_timestamp": 122, "json_lines": 0, "malformed_json": 0,
    "invalid_utf8": 0, "truncated": 0,
    "distinct_templates": 8, "template_overflow_lines": 0
  },
  "time_range": { "first": "2024-03-01T09:00:02Z", "last": "2024-03-01T09:59:02Z" },
  "levels": { "fatal": 0, "error": 15, "warn": 5, "info": 90, "debug": 12, "trace": 0, "unknown": 2 },
  "histogram": {
    "bucket_seconds": 300,
    "buckets": [{ "start": "2024-03-01T09:00:00Z", "count": 9, "errors": 0 }]
  },
  "templates": [
    {
      "template": "<ts> INFO  [http] GET /api/users/<num> completed in <dur> status=<num>",
      "count": 60,
      "share": 0.4838709677419355,
      "level": "info",
      "first_seen": "2024-03-01T09:00:02Z",
      "last_seen": "2024-03-01T09:59:02Z",
      "first_line": 1,
      "last_line": 124,
      "example": "2024-03-01T09:00:02Z INFO  [http] GET /api/users/17 completed in 8ms status=200"
    }
  ],
  "bursts": [
    {
      "start": "2024-03-01T09:40:00Z", "end": "2024-03-01T09:45:00Z",
      "errors": 14, "lines": 23, "peak_errors": 14, "baseline_errors": 0
    }
  ]
}
```

(Shortened: the real output has all 12 buckets and 8 templates.) `share` is
a fraction of matched lines. `level` is the level of the first line with
that template, or `null`. `first_seen` and `last_seen` are `null` for a
template whose lines have no timestamp; `first_line` and `last_line` are
1-based line numbers across all inputs and are always present. A test pins
this schema.

## Performance

`bench/run.sh` builds a release binary, generates a synthetic log, and times
the tool on it with `/usr/bin/time`. The generator is
`examples/genlog.rs`; its header lists the line kinds and their shares. The
log depends only on the line count and the seed (5,000,000 lines and 42 by
default), so the input can be reproduced exactly.

One run on an Apple M1 Max laptop, with the exact command:

```console
$ bench/run.sh
log:      bench/data/synthetic-5000000-42.log
lines:    5000000
bytes:    508118548
command:  target/release/logshape --json bench/data/synthetic-5000000-42.log > /dev/null

run 1:  wall 10.94 s  cpu 8.18 s  457038 lines/s  46.4 MB/s  peak RSS 2.8 MiB
run 2:  wall 9.36 s  cpu 7.64 s  534188 lines/s  54.3 MB/s  peak RSS 2.8 MiB
run 3:  wall 11.81 s  cpu 8.56 s  423370 lines/s  43.0 MB/s  peak RSS 3.1 MiB
run 4:  wall 12.37 s  cpu 7.49 s  404204 lines/s  41.1 MB/s  peak RSS 2.8 MiB
run 5:  wall 4.37 s  cpu 3.92 s  1144165 lines/s  116.3 MB/s  peak RSS 2.8 MiB

median:  10.94 s  457038 lines/s  46.4 MB/s

reference:  wc -l          wall 0.74 s  cpu 0.65 s
reference:  grep -c ERROR  wall 3.10 s  cpu 2.77 s
```

How to read this:

- **Memory is the solid result.** Peak resident set size was about 3 MiB
  for a 508 MB file in every run. That is the buffers plus 33 templates;
  it does not depend on the size of the input.
- **Throughput here is a lower bound, and a noisy one.** The laptop was busy
  with unrelated work during these runs (load average between 50 and 90),
  which is why the runs differ by almost a factor of three. The median was
  457,000 lines/s (46 MB/s) and the best run 1,144,000 lines/s (116 MB/s).
  I have not measured it on an idle machine, so I make no claim about that.
- The two reference lines put the same file through `wc -l` and
  `grep -c ERROR` right after the measured runs, as a rough gauge of the
  machine at that moment. They are not a like-for-like comparison: those
  tools do far less work per line.

MB means 10^6 bytes. The file is read from the page cache (one unmeasured
run comes first), so this measures the tool, not the disk.

## Tests

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Unit tests cover timestamp and level detection, every masking rule, the
JSON field extraction, the line reader and the histogram. The end-to-end
tests in `tests/cli.rs` run the compiled binary on the logs in
`tests/fixtures`: an application log, an access log, a syslog file, a
JSON-lines file with broken entries, and `messy.log`, which has invalid
UTF-8, binary bytes, blank lines, CRLF endings, impossible dates and
unclosed quotes. CI runs the same three commands on Ubuntu.

## Limitations

- Templates come from fixed masking rules, not from learning. A variable
  part made of plain words (a user name, a hostname, a URL slug) is not
  masked, and each value becomes its own template. The cap keeps memory
  safe when that happens, and the summary says how many lines were not
  grouped, but the template list is less useful for such logs.
- Access logs have no level, so they produce no error counts and no bursts.
  The status code is kept in the template but is not read as a level.
- Multi-line records are not joined. Each line of a stack trace is counted
  on its own, has no timestamp, and is dropped by `--since` and `--until`.
- Timestamps are found only in the first 64 bytes of a line. Syslog
  timestamps have no year: the tool uses `--year` or the current year, and
  does not guess across a new year.
- Zone names (`CET`, `PST`) are not understood, only numeric offsets and
  `Z`. A timestamp without an offset is treated as UTC.
- One wrong timestamp far from the rest (a 1970 date in an otherwise 2024
  log) stretches the automatic histogram to cover it. Use `--since` to cut
  it off.
- With `--bucket`, the histogram is not merged, so memory grows with the
  number of non-empty buckets. A one-second bucket over a year of logs is a
  lot of buckets.
- It reads on one thread.

## Licence

Apache-2.0. See [LICENSE](LICENSE).
