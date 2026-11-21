//! Timestamp detection.
//!
//! Recognised forms, all resolved to whole seconds since the Unix epoch:
//!
//! | Form                   | Example                               |
//! |------------------------|---------------------------------------|
//! | ISO 8601 / RFC 3339    | `2024-01-15T10:30:00.123+02:00`       |
//! | ISO-like with a space  | `2024-01-15 10:30:00,123`             |
//! | Slash dates (nginx)    | `2024/01/15 10:30:00`                 |
//! | Common Log Format      | `15/Jan/2024:10:30:00 +0000`          |
//! | Syslog (RFC 3164)      | `Jan  5 10:30:00`                     |
//! | ctime (Apache errors)  | `Mon Jan 15 10:30:00.123456 2024`     |
//!
//! A timestamp without a zone is read as UTC. Syslog timestamps carry no
//! year, so the caller supplies one. Fractions of a second are dropped.

/// Only timestamps that start within this many bytes of the line are found.
pub const SCAN_WINDOW: usize = 64;

/// A timestamp located inside a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    /// Seconds since the Unix epoch, UTC.
    pub epoch: i64,
    /// Byte offset of the first byte of the timestamp.
    pub start: usize,
    /// Byte offset one past the last byte of the timestamp.
    pub end: usize,
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: `(year, month, day)`.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Formats an epoch as `2024-01-15T10:30:00Z`.
pub fn format_rfc3339(epoch: i64) -> String {
    let (y, mo, d, h, mi, s) = split(epoch);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Formats an epoch as `2024-01-15 10:30:00`.
pub fn format_plain(epoch: i64) -> String {
    let (y, mo, d, h, mi, s) = split(epoch);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

fn split(epoch: i64) -> (i64, u32, u32, i64, i64, i64) {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    (y, mo, d, secs / 3600, secs % 3600 / 60, secs % 60)
}

/// The UTC year of an epoch.
pub fn year_of(epoch: i64) -> i32 {
    civil_from_days(epoch.div_euclid(86_400)).0 as i32
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

fn epoch_from(y: i64, mo: u32, d: u32, secs_of_day: i64, offset: i64) -> Option<i64> {
    if !(1900..=9999).contains(&y) || !(1..=12).contains(&mo) {
        return None;
    }
    if d == 0 || d > days_in_month(y, mo) {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + secs_of_day - offset)
}

/// Exactly `n` ASCII digits at `i`.
fn num(b: &[u8], i: usize, n: usize) -> Option<u32> {
    let s = b.get(i..i + n)?;
    let mut v = 0u32;
    for &c in s {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + u32::from(c - b'0');
    }
    Some(v)
}

fn month_abbr(b: &[u8], i: usize) -> Option<u32> {
    let m = match b.get(i..i + 3)? {
        b"Jan" => 1,
        b"Feb" => 2,
        b"Mar" => 3,
        b"Apr" => 4,
        b"May" => 5,
        b"Jun" => 6,
        b"Jul" => 7,
        b"Aug" => 8,
        b"Sep" => 9,
        b"Oct" => 10,
        b"Nov" => 11,
        b"Dec" => 12,
        _ => return None,
    };
    Some(m)
}

fn is_weekday(b: &[u8], i: usize) -> bool {
    matches!(
        b.get(i..i + 3),
        Some(b"Mon" | b"Tue" | b"Wed" | b"Thu" | b"Fri" | b"Sat" | b"Sun")
    )
}

/// `HH:MM[:SS][.fff]` at `i`. Returns seconds of day and the end offset.
fn parse_time(b: &[u8], i: usize, need_seconds: bool) -> Option<(i64, usize)> {
    let h = num(b, i, 2)?;
    if b.get(i + 2) != Some(&b':') {
        return None;
    }
    let m = num(b, i + 3, 2)?;
    let mut p = i + 5;
    let mut s = 0;
    if b.get(p) == Some(&b':') && num(b, p + 1, 2).is_some() {
        s = num(b, p + 1, 2)?;
        p += 3;
        if matches!(b.get(p), Some(b'.' | b',')) && b.get(p + 1).is_some_and(u8::is_ascii_digit) {
            p += 1;
            while b.get(p).is_some_and(u8::is_ascii_digit) {
                p += 1;
            }
        }
    } else if need_seconds {
        return None;
    }
    if h > 23 || m > 59 || s > 60 {
        return None;
    }
    if b.get(p).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some((i64::from(h * 3600 + m * 60 + s), p))
}

/// Zone designator at `p`: `Z`, `+HH:MM`, `+HHMM`, `+HH`, or a single space
/// followed by `+HHMM`. Returns the offset in seconds and the end offset; a
/// missing zone is `(0, p)`.
fn parse_zone(b: &[u8], p: usize) -> (i64, usize) {
    match b.get(p) {
        Some(b'Z' | b'z') if !b.get(p + 1).copied().is_some_and(is_word) => (0, p + 1),
        Some(b'+' | b'-') => signed_zone(b, p, false).unwrap_or((0, p)),
        Some(b' ') => signed_zone(b, p + 1, true).unwrap_or((0, p)),
        _ => (0, p),
    }
}

fn signed_zone(b: &[u8], p: usize, strict: bool) -> Option<(i64, usize)> {
    let sign = match b.get(p)? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let h = num(b, p + 1, 2)?;
    let (m, end) = if let Some(m) = num(b, p + 3, 2) {
        (m, p + 5)
    } else if strict {
        return None;
    } else if b.get(p + 3) == Some(&b':') {
        (num(b, p + 4, 2)?, p + 6)
    } else {
        (0, p + 3)
    };
    if h > 14 || m > 59 || b.get(end).copied().is_some_and(is_word) {
        return None;
    }
    Some((sign * i64::from(h * 3600 + m * 60), end))
}

/// ISO 8601 / RFC 3339 and close relatives at `i`. With `need_time` false a
/// bare date is accepted and read as midnight.
pub fn parse_iso(b: &[u8], i: usize, need_time: bool) -> Option<(i64, usize)> {
    let y = num(b, i, 4)?;
    let sep = *b.get(i + 4)?;
    if sep != b'-' && sep != b'/' {
        return None;
    }
    let mo = num(b, i + 5, 2)?;
    if b.get(i + 7) != Some(&sep) {
        return None;
    }
    let d = num(b, i + 8, 2)?;
    let p = i + 10;
    if matches!(b.get(p), Some(b'T' | b't' | b' ')) {
        if let Some((secs, end)) = parse_time(b, p + 1, false) {
            let (offset, end) = parse_zone(b, end);
            return Some((epoch_from(i64::from(y), mo, d, secs, offset)?, end));
        }
    }
    if need_time || b.get(p).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some((epoch_from(i64::from(y), mo, d, 0, 0)?, p))
}

/// Common Log Format at `i`: `15/Jan/2024:10:30:00 +0000`.
pub fn parse_clf(b: &[u8], i: usize) -> Option<(i64, usize)> {
    let d = num(b, i, 2)?;
    if b.get(i + 2) != Some(&b'/') {
        return None;
    }
    let mo = month_abbr(b, i + 3)?;
    if b.get(i + 6) != Some(&b'/') {
        return None;
    }
    let y = num(b, i + 7, 4)?;
    if b.get(i + 11) != Some(&b':') {
        return None;
    }
    let (secs, end) = parse_time(b, i + 12, true)?;
    let (offset, end) = parse_zone(b, end);
    Some((epoch_from(i64::from(y), mo, d, secs, offset)?, end))
}

/// Syslog at `i`: `Jan  5 10:30:00`, with the year supplied by the caller.
pub fn parse_syslog(b: &[u8], i: usize, year: i32) -> Option<(i64, usize)> {
    let (mo, d, secs, end) = month_day_time(b, i)?;
    Some((epoch_from(i64::from(year), mo, d, secs, 0)?, end))
}

/// ctime at `i`: `Mon Jan 15 10:30:00.123456 2024`. The year falls back to
/// `year` when the line does not carry one.
pub fn parse_ctime(b: &[u8], i: usize, year: i32) -> Option<(i64, usize)> {
    if !is_weekday(b, i) || b.get(i + 3) != Some(&b' ') {
        return None;
    }
    let (mo, d, secs, mut end) = month_day_time(b, i + 4)?;
    let mut y = i64::from(year);
    if b.get(end) == Some(&b' ') {
        if let Some(v) = num(b, end + 1, 4) {
            if !b.get(end + 5).copied().is_some_and(is_word) {
                y = i64::from(v);
                end += 5;
            }
        }
    }
    Some((epoch_from(y, mo, d, secs, 0)?, end))
}

fn month_day_time(b: &[u8], i: usize) -> Option<(u32, u32, i64, usize)> {
    let mo = month_abbr(b, i)?;
    let mut p = i + 3;
    if b.get(p) != Some(&b' ') {
        return None;
    }
    p += 1;
    if b.get(p) == Some(&b' ') {
        p += 1;
    }
    let d = if let Some(d) = num(b, p, 2) {
        p += 2;
        d
    } else {
        let d = num(b, p, 1)?;
        p += 1;
        d
    };
    if b.get(p) != Some(&b' ') {
        return None;
    }
    let (secs, end) = parse_time(b, p + 1, true)?;
    Some((mo, d, secs, end))
}

/// Finds the first timestamp that starts within [`SCAN_WINDOW`] bytes.
pub fn find(line: &[u8], year: i32) -> Option<Found> {
    let limit = line.len().min(SCAN_WINDOW);
    for i in 0..limit {
        if i > 0 && is_word(line[i - 1]) {
            continue;
        }
        let c = line[i];
        let hit = if c.is_ascii_digit() {
            parse_iso(line, i, true).or_else(|| parse_clf(line, i))
        } else if c.is_ascii_uppercase() {
            parse_ctime(line, i, year).or_else(|| parse_syslog(line, i, year))
        } else {
            None
        };
        if let Some((epoch, end)) = hit {
            return Some(Found {
                epoch,
                start: i,
                end,
            });
        }
    }
    None
}

/// Parses a whole string as a timestamp, for `--since` and `--until` and for
/// string fields in JSON lines. Bare dates are read as midnight UTC.
pub fn parse_whole(s: &str, year: i32) -> Option<i64> {
    let b = s.trim().as_bytes();
    let hit = parse_iso(b, 0, false)
        .or_else(|| parse_clf(b, 0))
        .or_else(|| parse_ctime(b, 0, year))
        .or_else(|| parse_syslog(b, 0, year))?;
    (hit.1 == b.len()).then_some(hit.0)
}

/// Interprets a numeric JSON field as an epoch in seconds, milliseconds,
/// microseconds or nanoseconds, judged by magnitude.
pub fn from_number(v: f64) -> Option<i64> {
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    let secs = if v >= 1e17 {
        v / 1e9
    } else if v >= 1e14 {
        v / 1e6
    } else if v >= 1e11 {
        v / 1e3
    } else {
        v
    };
    Some(secs as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_705_314_600; // 2024-01-15T10:30:00Z

    fn found(line: &str) -> Option<(i64, &str)> {
        find(line.as_bytes(), 2024).map(|f| (f.epoch, &line[f.start..f.end]))
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2024, 1, 15), 19_737);
        for days in [-1, 0, 59, 60, 11_016, 19_737, 19_782, 47_482] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(format_rfc3339(T), "2024-01-15T10:30:00Z");
        assert_eq!(format_plain(T), "2024-01-15 10:30:00");
        assert_eq!(year_of(T), 2024);
    }

    #[test]
    fn rfc3339() {
        assert_eq!(
            found("2024-01-15T10:30:00Z INFO up"),
            Some((T, "2024-01-15T10:30:00Z"))
        );
        assert_eq!(
            found("2024-01-15T12:30:00.123+02:00 up"),
            Some((T, "2024-01-15T12:30:00.123+02:00"))
        );
        assert_eq!(
            found("2024-01-15T05:30:00-0500 up"),
            Some((T, "2024-01-15T05:30:00-0500"))
        );
    }

    #[test]
    fn iso_with_space_and_comma_fraction() {
        assert_eq!(
            found("2024-01-15 10:30:00,123 [main] INFO up"),
            Some((T, "2024-01-15 10:30:00,123"))
        );
        assert_eq!(
            found("2024-01-15 11:30:00 +0100 up"),
            Some((T, "2024-01-15 11:30:00 +0100"))
        );
        assert_eq!(found("2024-01-15 10:30 up"), Some((T, "2024-01-15 10:30")));
    }

    #[test]
    fn zone_is_not_taken_from_following_text() {
        assert_eq!(
            found("2024-01-15 10:30:00 -5 items"),
            Some((T, "2024-01-15 10:30:00"))
        );
        assert_eq!(
            found("2024-01-15 10:30:00-worker up"),
            Some((T, "2024-01-15 10:30:00"))
        );
    }

    #[test]
    fn nginx_error_log() {
        assert_eq!(
            found("2024/01/15 10:30:00 [error] 12#12: boom"),
            Some((T, "2024/01/15 10:30:00"))
        );
    }

    #[test]
    fn common_log_format() {
        assert_eq!(
            found(r#"10.0.0.1 - - [15/Jan/2024:10:30:00 +0000] "GET / HTTP/1.1" 200 5"#),
            Some((T, "15/Jan/2024:10:30:00 +0000"))
        );
        assert_eq!(
            found("[15/Jan/2024:11:30:00 +0100] x"),
            Some((T, "15/Jan/2024:11:30:00 +0100"))
        );
    }

    #[test]
    fn syslog_uses_supplied_year() {
        assert_eq!(
            found("Jan 15 10:30:00 host sshd[42]: ok"),
            Some((T, "Jan 15 10:30:00"))
        );
        let padded = found("Jan  5 10:30:00 host x").unwrap();
        assert_eq!(padded.0, T - 10 * 86_400);
        assert_eq!(padded.1, "Jan  5 10:30:00");
        let other_year = find(b"Jan 15 10:30:00 host x", 2023).unwrap();
        assert_eq!(other_year.epoch, T - 365 * 86_400);
    }

    #[test]
    fn ctime_with_and_without_year() {
        assert_eq!(
            found("[Mon Jan 15 10:30:00.123456 2024] [core:error] boom"),
            Some((T, "Mon Jan 15 10:30:00.123456 2024"))
        );
        assert_eq!(
            found("Mon Jan 15 10:30:00 up"),
            Some((T, "Mon Jan 15 10:30:00"))
        );
    }

    #[test]
    fn rejects_impossible_values() {
        assert_eq!(found("2024-13-01 10:30:00 x"), None);
        assert_eq!(found("2024-02-30 10:30:00 x"), None);
        assert_eq!(found("2024-01-15 25:30:00 x"), None);
        assert_eq!(found("2023-02-29T00:00:00Z x"), None);
        assert!(found("2024-02-29T00:00:00Z x").is_some());
        assert_eq!(found("Jan 32 10:30:00 x"), None);
    }

    #[test]
    fn needs_a_time_and_a_token_boundary() {
        assert_eq!(found("released 2024-01-15 as planned"), None);
        assert_eq!(found("id=x2024-01-15T10:30:00Z"), None);
        assert_eq!(found("no timestamp here"), None);
        assert_eq!(found(""), None);
    }

    #[test]
    fn only_scans_the_start_of_the_line() {
        let far = format!("{} 2024-01-15T10:30:00Z", "x".repeat(SCAN_WINDOW));
        assert_eq!(found(&far), None);
        let near = format!("{} 2024-01-15T10:30:00Z", "x".repeat(SCAN_WINDOW - 2));
        assert!(found(&near).is_some());
    }

    #[test]
    fn whole_string_parsing() {
        assert_eq!(parse_whole("2024-01-15T10:30:00Z", 2024), Some(T));
        assert_eq!(parse_whole(" 2024-01-15 10:30:00 ", 2024), Some(T));
        assert_eq!(parse_whole("2024-01-15", 2024), Some(T - 37_800));
        assert_eq!(parse_whole("Jan 15 10:30:00", 2024), Some(T));
        assert_eq!(parse_whole("2024-01-15T10:30:00Z tail", 2024), None);
        assert_eq!(parse_whole("yesterday", 2024), None);
    }

    #[test]
    fn numeric_epochs_by_magnitude() {
        assert_eq!(from_number(1_705_314_600.0), Some(T));
        assert_eq!(from_number(1_705_314_600.75), Some(T));
        assert_eq!(from_number(1_705_314_600_123.0), Some(T));
        assert_eq!(from_number(1_705_314_600_123_456.0), Some(T));
        assert_eq!(from_number(1.705_314_600_123_456_7e18), Some(T));
        assert_eq!(from_number(0.0), None);
        assert_eq!(from_number(f64::NAN), None);
    }
}
