//! Synthetic log generator for the benchmark.
//!
//! ```text
//! cargo run --release --example genlog -- --lines 5000000 --seed 42 > app.log
//! ```
//!
//! Output depends only on `--lines` and `--seed`: randomness comes from a
//! SplitMix64 generator written out below, not from the platform.
//!
//! The log starts at 2024-01-01T00:00:00Z and every line moves the clock
//! forward by 0 to 19 ms. Line kinds and their shares:
//!
//! | Share | Kind                                                    |
//! |-------|---------------------------------------------------------|
//! | 55%   | `INFO  [http]` request with method, path id, duration, status, client IP |
//! | 15%   | `DEBUG [cache]` hit or miss with a key and a size        |
//! | 10%   | `INFO  [worker]` job with a UUID and a duration          |
//! | 8%    | `WARN  [db]` slow query with a quoted SQL string         |
//! | 5%    | JSON line with `time`, `level`, `msg` and a hex session id |
//! | 4%    | `ERROR [db]` refused connection with an IP and port      |
//! | 3%    | stack trace line without timestamp or level              |
//!
//! Between 60% and 62% of the way through, the error share rises from 4% to
//! about 40%, which gives burst detection something to find.

use std::io::{self, BufWriter, Write};

use logshape::timestamp::civil_from_days;

/// 2024-01-01 as days since the Unix epoch.
const START_DAY: i64 = 19_723;

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (the tiny modulo bias does not matter here).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

const METHODS: [&str; 4] = ["GET", "GET", "GET", "POST"];
const RESOURCES: [&str; 5] = ["users", "orders", "invoices", "sessions", "reports"];
const STATUSES: [&str; 8] = ["200", "200", "200", "200", "201", "204", "404", "500"];
const TABLES: [&str; 3] = ["orders", "users", "events"];
const CLASSES: [&str; 4] = ["Pool", "Handler", "Dispatcher", "Session"];
const CALLS: [&str; 4] = ["acquire", "handle", "dispatch", "refresh"];

fn usage() -> ! {
    eprintln!("usage: genlog [--lines N] [--seed N]");
    std::process::exit(2);
}

fn main() -> io::Result<()> {
    let mut lines: u64 = 1_000_000;
    let mut seed: u64 = 42;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().and_then(|v| v.parse::<u64>().ok());
        match (flag.as_str(), value) {
            ("--lines", Some(v)) => lines = v,
            ("--seed", Some(v)) => seed = v,
            _ => usage(),
        }
    }

    let mut rng = SplitMix64(seed);
    let mut out = BufWriter::with_capacity(1 << 20, io::stdout().lock());
    // Milliseconds since 2024-01-01T00:00:00Z.
    let mut clock: u64 = 0;
    for n in 0..lines {
        clock += rng.below(20);
        let (day, ms) = (clock / 86_400_000, clock % 86_400_000);
        let (year, month, dom) = civil_from_days(START_DAY + day as i64);
        let ts = format!(
            "{year:04}-{month:02}-{dom:02}T{:02}:{:02}:{:02}.{:03}Z",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000
        );

        let percent_done = n * 100 / lines;
        let mut roll = rng.below(100);
        if (60..62).contains(&percent_done) && rng.below(100) < 38 {
            roll = 93; // an error
        }
        match roll {
            0..=54 => writeln!(
                out,
                "{ts} INFO  [http] {} /api/{}/{} completed in {}ms status={} client=10.{}.{}.{}",
                rng.pick(&METHODS),
                rng.pick(&RESOURCES),
                rng.below(1_000_000),
                1 + rng.below(900),
                rng.pick(&STATUSES),
                rng.below(256),
                rng.below(256),
                1 + rng.below(254)
            )?,
            55..=69 => writeln!(
                out,
                "{ts} DEBUG [cache] {} key=user:{}:profile size={}B",
                rng.pick(&["hit", "miss"]),
                rng.below(500_000),
                64 + rng.below(8000)
            )?,
            70..=79 => {
                let (a, b) = (rng.next(), rng.next());
                writeln!(
                    out,
                    "{ts} INFO  [worker] job {:08x}-{:04x}-4{:03x}-a{:03x}-{:012x} finished in {}.{}s",
                    a >> 32,
                    a >> 16 & 0xffff,
                    a & 0xfff,
                    b >> 48 & 0xfff,
                    b & 0xffff_ffff_ffff,
                    rng.below(30),
                    rng.below(10)
                )?
            }
            80..=87 => writeln!(
                out,
                "{ts} WARN  [db] slow query took {}ms: \"SELECT * FROM {} WHERE id = {}\"",
                500 + rng.below(5000),
                rng.pick(&TABLES),
                rng.below(1_000_000)
            )?,
            88..=92 => writeln!(
                out,
                "{{\"time\":\"{ts}\",\"level\":\"info\",\"msg\":\"session {:016x} refreshed\",\"user\":{}}}",
                rng.next() | 1 << 63,
                rng.below(500_000)
            )?,
            93..=96 => writeln!(
                out,
                "{ts} ERROR [db] connection to 10.2.0.{}:5432 refused (attempt {})",
                1 + rng.below(8),
                1 + rng.below(5)
            )?,
            _ => {
                let class = rng.pick(&CLASSES);
                writeln!(
                    out,
                    "\tat com.example.{class}.{}({class}.java:{})",
                    rng.pick(&CALLS),
                    1 + rng.below(400)
                )?
            }
        }
    }
    out.flush()
}
