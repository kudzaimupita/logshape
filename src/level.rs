//! Log level detection.
//!
//! Six levels are reported. Common spellings are folded into them:
//!
//! | Level | Spellings                                         |
//! |-------|---------------------------------------------------|
//! | fatal | `FATAL` `CRITICAL` `CRIT` `PANIC` `EMERG` `ALERT` |
//! | error | `ERROR` `ERR` `SEVERE`                            |
//! | warn  | `WARN` `WARNING`                                  |
//! | info  | `INFO` `NOTICE`                                   |
//! | debug | `DEBUG` `DBG`                                     |
//! | trace | `TRACE` `VERBOSE`                                 |
//!
//! In plain text an upper-case level word is accepted wherever it appears in
//! the scan window. Lower-case and capitalised words are common in prose
//! ("no error found"), so they only count when the line marks them as a
//! level: wrapped in brackets (`[error]`), followed by a colon (`error:`),
//! or given as a value (`level=error`, `"level":"error"`).

use std::fmt;
use std::str::FromStr;

/// The level is searched for within this many bytes after the timestamp (or
/// from the start of the line when there is none).
pub const SCAN_WINDOW: usize = 96;

/// Severity, ordered from least to most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    /// Most severe first, the order used in reports.
    pub const ALL: [Level; 6] = [
        Level::Fatal,
        Level::Error,
        Level::Warn,
        Level::Info,
        Level::Debug,
        Level::Trace,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// Error and fatal lines feed the error counts and burst detection.
    pub fn is_error(self) -> bool {
        self >= Level::Error
    }

    /// Maps a level word in any letter case.
    pub fn from_word(word: &[u8]) -> Option<Level> {
        if !(3..=8).contains(&word.len()) {
            return None;
        }
        let mut buf = [0u8; 8];
        let lower = &mut buf[..word.len()];
        lower.copy_from_slice(word);
        lower.make_ascii_lowercase();
        Some(match &*lower {
            b"fatal" | b"critical" | b"crit" | b"panic" | b"emerg" | b"alert" => Level::Fatal,
            b"error" | b"err" | b"severe" => Level::Error,
            b"warn" | b"warning" => Level::Warn,
            b"info" | b"notice" => Level::Info,
            b"debug" | b"dbg" => Level::Debug,
            b"trace" | b"verbose" => Level::Trace,
            _ => return None,
        })
    }

    /// Maps the numeric levels used by pino and bunyan (10 to 60).
    pub fn from_number(n: f64) -> Option<Level> {
        Some(match n as i64 {
            10 => Level::Trace,
            20 => Level::Debug,
            30 => Level::Info,
            40 => Level::Warn,
            50 => Level::Error,
            60 => Level::Fatal,
            _ => return None,
        })
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for Level {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Level::from_word(s.as_bytes()).ok_or_else(|| {
            format!("unknown level '{s}' (expected trace, debug, info, warn, error or fatal)")
        })
    }
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Finds the level of a plain-text line. `skip` is the byte range of the
/// timestamp, which is never searched.
pub fn detect(line: &[u8], skip: Option<(usize, usize)>) -> Option<Level> {
    let limit = line.len().min(skip.map_or(0, |s| s.1) + SCAN_WINDOW);
    let mut i = 0;
    while i < limit {
        if let Some((start, end)) = skip {
            if i == start {
                i = end;
                continue;
            }
        }
        if !is_word(line[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < line.len() && is_word(line[i]) {
            i += 1;
        }
        let word = &line[start..i];
        if let Some(level) = Level::from_word(word) {
            if word.iter().all(u8::is_ascii_uppercase) || is_marked(line, start, i) {
                return Some(level);
            }
        }
    }
    None
}

/// Whether the word at `start..end` is set apart as a level by punctuation.
fn is_marked(line: &[u8], start: usize, end: usize) -> bool {
    let word = &line[start..end];
    let lower = word.iter().all(u8::is_ascii_lowercase);
    let capitalised = word[0].is_ascii_uppercase() && word[1..].iter().all(u8::is_ascii_lowercase);
    if !lower && !capitalised {
        return false;
    }
    let before = start.checked_sub(1).map(|p| line[p]);
    let after = line.get(end).copied();
    if after == Some(b':') {
        return true;
    }
    // `[error]`, `<warn>`, `(info)` and module-qualified `[core:error]`.
    if matches!(before, Some(b'[' | b'<' | b'(' | b':'))
        && matches!(after, Some(b']' | b'>' | b')'))
    {
        return true;
    }
    is_level_value(&line[..start])
}

/// Whether `prefix` ends with a key such as `level=`, `lvl=` or `severity=`,
/// allowing `:` for `=` and quotes around either side (`"level":"`).
fn is_level_value(prefix: &[u8]) -> bool {
    let prefix = prefix.strip_suffix(b"\"").unwrap_or(prefix);
    let Some(prefix) = prefix
        .strip_suffix(b"=")
        .or_else(|| prefix.strip_suffix(b":"))
    else {
        return false;
    };
    let prefix = prefix.strip_suffix(b"\"").unwrap_or(prefix);
    [&b"level"[..], b"lvl", b"severity"].iter().any(|key| {
        prefix.len() >= key.len() && prefix[prefix.len() - key.len()..].eq_ignore_ascii_case(key)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &str) -> Option<Level> {
        detect(line.as_bytes(), None)
    }

    #[test]
    fn upper_case_words() {
        assert_eq!(plain("ERROR boom"), Some(Level::Error));
        assert_eq!(plain("[WARN] slow"), Some(Level::Warn));
        assert_eq!(plain("12:00 INFO  [main] ready"), Some(Level::Info));
        assert_eq!(plain("DEBUG: x"), Some(Level::Debug));
        assert_eq!(plain("TRACE enter"), Some(Level::Trace));
        assert_eq!(plain("FATAL out of memory"), Some(Level::Fatal));
    }

    #[test]
    fn variants_fold_into_six_levels() {
        for (word, level) in [
            ("WARNING", Level::Warn),
            ("ERR", Level::Error),
            ("SEVERE", Level::Error),
            ("CRITICAL", Level::Fatal),
            ("CRIT", Level::Fatal),
            ("PANIC", Level::Fatal),
            ("EMERG", Level::Fatal),
            ("ALERT", Level::Fatal),
            ("NOTICE", Level::Info),
            ("DBG", Level::Debug),
            ("VERBOSE", Level::Trace),
        ] {
            assert_eq!(plain(&format!("x {word} y")), Some(level), "{word}");
        }
    }

    #[test]
    fn lower_case_needs_a_marker() {
        assert_eq!(plain("no error found in 3 files"), None);
        assert_eq!(plain("Warning lights are on"), None);
        assert_eq!(
            plain("[error] 12#12: upstream timed out"),
            Some(Level::Error)
        );
        assert_eq!(plain("<warn> disk"), Some(Level::Warn));
        assert_eq!(plain("error: cannot open file"), Some(Level::Error));
        assert_eq!(plain("Warning: deprecated"), Some(Level::Warn));
        assert_eq!(plain("ts=1 level=info msg=ok"), Some(Level::Info));
        assert_eq!(plain("lvl=\"debug\" msg=ok"), Some(Level::Debug));
        assert_eq!(plain("log.level=warn"), Some(Level::Warn));
        assert_eq!(plain(r#"{"level":"error","msg":"cut"#), Some(Level::Error));
        assert_eq!(plain("severity: warning"), None);
        assert_eq!(plain("[core:error] [pid 1] boom"), Some(Level::Error));
    }

    #[test]
    fn whole_words_only() {
        assert_eq!(plain("ERR_CONNECTION_RESET seen"), None);
        assert_eq!(plain("INFORMATION desk"), None);
        assert_eq!(plain("XERROR"), None);
        assert_eq!(plain("ErRoR: odd"), None);
    }

    #[test]
    fn first_level_word_wins() {
        assert_eq!(plain("INFO retrying after ERROR"), Some(Level::Info));
    }

    #[test]
    fn timestamp_is_skipped_and_extends_the_window() {
        let line = "2024-01-15T10:30:00Z ERROR boom";
        assert_eq!(detect(line.as_bytes(), Some((0, 20))), Some(Level::Error));
        let far = format!("{} ERROR", "x".repeat(SCAN_WINDOW));
        assert_eq!(plain(&far), None);
    }

    #[test]
    fn numbers_and_parsing() {
        assert_eq!(Level::from_number(30.0), Some(Level::Info));
        assert_eq!(Level::from_number(50.0), Some(Level::Error));
        assert_eq!(Level::from_number(3.0), None);
        assert_eq!("Warning".parse::<Level>(), Ok(Level::Warn));
        assert!("loud".parse::<Level>().is_err());
        assert!(Level::Fatal.is_error() && Level::Error.is_error());
        assert!(!Level::Warn.is_error());
        assert!(Level::Warn > Level::Info);
    }
}
