//! Template masking: replaces the variable parts of a line with placeholders
//! so that lines of the same kind collapse into one template.
//!
//! The rules, in the order they are tried at each token boundary:
//!
//! | Placeholder | Matches                                                        |
//! |-------------|----------------------------------------------------------------|
//! | `<ts>`      | the detected line timestamp, and any other ISO 8601 timestamp  |
//! | `<str>`     | `"double quoted"` and `'single quoted'` strings                |
//! | `<uuid>`    | `8-4-4-4-12` hex UUIDs                                         |
//! | `<ip>`      | IPv4 with optional `:port` or `/prefix`; IPv6 with `::` or 8 groups |
//! | `<hex>`     | `0x…`, or 8+ hex characters with at least one digit and one letter |
//! | `<id>`      | other tokens of 8+ characters with letters and 3+ digits       |
//! | `<dur>`     | a number with a time unit: `ns us µs ms s sec m min h`         |
//! | `<num>`     | integers and decimals, also before a short unit (`<num>MB`)    |
//!
//! One exception keeps access logs useful. A quoted HTTP request line is not
//! reduced to `<str>`: its inside is masked like any other text, a query
//! string becomes `?<query>`, and a three-digit status code after it is kept:
//! `"GET /users/42?full=1 HTTP/1.1" 200 512` becomes
//! `"GET /users/<num>?<query> HTTP/<num>" 200 <num>`.
//!
//! A token is a run of letters, digits and `_`, so `/`, `-`, `=`, `.` and `:`
//! all separate tokens. That is what handles paths with ids:
//! `/users/42/orders/9f8e7d6c` becomes `/users/<num>/orders/<hex>`.
//!
//! Digits glued to letters in a short token are left alone, which keeps
//! words such as `http2`, `sha256`, `x86_64` and `utf8` readable.

use crate::timestamp;

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn word_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && is_word(b[i]) {
        i += 1;
    }
    i
}

fn boundary(b: &[u8], i: usize) -> bool {
    !b.get(i).copied().is_some_and(is_word)
}

/// Masks `line` into `out` (which is cleared first). `ts` is the byte range
/// of the timestamp found by [`timestamp::find`], if any.
pub fn mask_into(line: &str, ts: Option<(usize, usize)>, out: &mut String) {
    out.clear();
    mask_range(line, 0, line.len(), ts, out);
}

/// Masks `line[from..to]`, appending to `out`.
fn mask_range(line: &str, from: usize, to: usize, ts: Option<(usize, usize)>, out: &mut String) {
    let b = &line.as_bytes()[..to];
    let mut lit = from; // start of the literal text not yet copied
    let mut i = from;
    while i < to {
        let at_start = i == from || !is_word(b[i - 1]);
        if b[i] == b'"' {
            if let Some(close) = request_line(b, i) {
                // Keep the quotes, mask inside them, keep the status code.
                out.push_str(&line[lit..=i]);
                let query = memchr::memchr(b'?', &b[i..close]).map_or(close, |q| i + q);
                mask_range(line, i + 1, query, None, out);
                if query < close {
                    out.push_str("?<query>");
                    let rest = memchr::memchr(b' ', &b[query..close]).map_or(close, |s| query + s);
                    mask_range(line, rest, close, None, out);
                }
                lit = close;
                i = close + 1;
                if b.get(i) == Some(&b' ')
                    && b.get(i + 1..i + 4)
                        .is_some_and(|s| s.iter().all(u8::is_ascii_digit))
                    && boundary(b, i + 4)
                {
                    i += 4;
                }
                continue;
            }
        }
        let hit = match ts {
            Some((start, end)) if i == start => Some(("<ts>", end)),
            _ => match b[i] {
                b'"' => quoted(b, i, b'"').map(|e| ("<str>", e)),
                b'\'' if at_start => quoted(b, i, b'\'').map(|e| ("<str>", e)),
                b'0'..=b'9' if at_start => digit_token(b, i),
                b':' if at_start => ipv6(b, i).map(|e| ("<ip>", e)),
                c if at_start && is_word(c) => letter_token(b, i),
                _ => None,
            },
        };
        match hit {
            Some((placeholder, end)) => {
                out.push_str(&line[lit..i]);
                out.push_str(placeholder);
                i = end;
                lit = end;
            }
            // Skip the rest of an unmasked token in one step.
            None if at_start && is_word(b[i]) => i = word_end(b, i),
            None => i += 1,
        }
    }
    out.push_str(&line[lit..to]);
}

/// If a quoted HTTP request line (`"GET /path HTTP/1.1"`) opens at `i`,
/// returns the offset of its closing quote.
fn request_line(b: &[u8], i: usize) -> Option<usize> {
    const METHODS: [&[u8]; 7] = [
        b"GET ",
        b"POST ",
        b"PUT ",
        b"DELETE ",
        b"HEAD ",
        b"PATCH ",
        b"OPTIONS ",
    ];
    let rest = &b[i + 1..];
    if !METHODS.iter().any(|m| rest.starts_with(m)) {
        return None;
    }
    quoted(b, i, b'"').map(|end| end - 1)
}

/// Convenience wrapper around [`mask_into`] for lines without a timestamp.
pub fn mask(line: &str) -> String {
    let mut out = String::new();
    mask_into(line, None, &mut out);
    out
}

/// End of the quoted string opening at `i`. A closing single quote must not
/// be followed by a letter, so apostrophes inside the string do not end it.
fn quoted(b: &[u8], i: usize, quote: u8) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            c if c == quote && (quote == b'"' || boundary(b, j + 1)) => return Some(j + 1),
            _ => {}
        }
        j += 1;
    }
    None
}

fn hex_run(b: &[u8], i: usize, n: usize) -> bool {
    b.get(i..i + n)
        .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
}

fn uuid(b: &[u8], i: usize) -> Option<usize> {
    let mut p = i;
    for (k, len) in [8, 4, 4, 4, 12].into_iter().enumerate() {
        if k > 0 {
            if b.get(p) != Some(&b'-') {
                return None;
            }
            p += 1;
        }
        if !hex_run(b, p, len) {
            return None;
        }
        p += len;
    }
    boundary(b, p).then_some(p)
}

/// Up to `max` digits at `i`: `(value, end)`.
fn digits(b: &[u8], i: usize, max: usize) -> Option<(u32, usize)> {
    let mut p = i;
    let mut v = 0u32;
    while p < b.len() && p - i < max && b[p].is_ascii_digit() {
        v = v * 10 + u32::from(b[p] - b'0');
        p += 1;
    }
    (p > i).then_some((v, p))
}

fn ipv4(b: &[u8], i: usize) -> Option<usize> {
    let mut p = i;
    for k in 0..4 {
        if k > 0 {
            if b.get(p) != Some(&b'.') {
                return None;
            }
            p += 1;
        }
        let (v, end) = digits(b, p, 3)?;
        if v > 255 {
            return None;
        }
        p = end;
    }
    // A fifth dotted group means this is something else, such as a version.
    if b.get(p).is_some_and(u8::is_ascii_digit)
        || (b.get(p) == Some(&b'.') && b.get(p + 1).is_some_and(u8::is_ascii_digit))
    {
        return None;
    }
    if matches!(b.get(p), Some(b':' | b'/')) {
        let max = if b[p] == b':' { 5 } else { 2 };
        if let Some((_, end)) = digits(b, p + 1, max) {
            if boundary(b, end) {
                p = end;
            }
        }
    }
    boundary(b, p).then_some(p)
}

/// IPv6 in full (eight groups) or compressed (`::`) form.
fn ipv6(b: &[u8], i: usize) -> Option<usize> {
    let mut p = i;
    let mut colons = 0;
    let mut hex = 0;
    let mut group = 0;
    let mut compressed = false;
    while p < b.len() {
        match b[p] {
            b':' => {
                colons += 1;
                group = 0;
                compressed |= b.get(p + 1) == Some(&b':');
            }
            c if c.is_ascii_hexdigit() => {
                hex += 1;
                group += 1;
                if group > 4 {
                    return None;
                }
            }
            _ => break,
        }
        p += 1;
    }
    // A single trailing colon is punctuation, not part of the address.
    if b[p - 1] == b':' && !(p >= i + 2 && b[p - 2] == b':') {
        p -= 1;
        colons -= 1;
    }
    let shaped = (compressed && colons >= 2) || colons == 7;
    (shaped && hex > 0 && boundary(b, p)).then_some(p)
}

fn is_duration_unit(unit: &[u8]) -> bool {
    matches!(
        unit,
        b"ns" | b"us" | b"\xc2\xb5s" | b"ms" | b"s" | b"sec" | b"m" | b"min" | b"h"
    )
}

/// `<hex>` or `<id>` for a whole token, or `None` to leave it as it is.
fn classify(word: &[u8]) -> Option<&'static str> {
    if let Some(rest) = word
        .strip_prefix(b"0x")
        .or_else(|| word.strip_prefix(b"0X"))
    {
        if !rest.is_empty() && rest.iter().all(u8::is_ascii_hexdigit) {
            return Some("<hex>");
        }
    }
    if word.len() < 8 {
        return None;
    }
    let digits = word.iter().filter(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits == word.len() {
        return None;
    }
    if word.iter().all(u8::is_ascii_hexdigit) {
        return Some("<hex>");
    }
    (digits >= 3).then_some("<id>")
}

/// A token that starts with a letter or `_`.
fn letter_token(b: &[u8], i: usize) -> Option<(&'static str, usize)> {
    if b[i].is_ascii_hexdigit() {
        if let Some(end) = uuid(b, i) {
            return Some(("<uuid>", end));
        }
        if let Some(end) = ipv6(b, i) {
            return Some(("<ip>", end));
        }
    }
    let end = word_end(b, i);
    classify(&b[i..end]).map(|p| (p, end))
}

/// A token that starts with a digit.
fn digit_token(b: &[u8], i: usize) -> Option<(&'static str, usize)> {
    if let Some(end) = uuid(b, i) {
        return Some(("<uuid>", end));
    }
    if matches!(b.get(i + 4), Some(b'-' | b'/')) {
        if let Some((_, end)) = timestamp::parse_iso(b, i, true) {
            return Some(("<ts>", end));
        }
    }
    if let Some(end) = ipv4(b, i) {
        return Some(("<ip>", end));
    }
    if let Some(end) = ipv6(b, i) {
        return Some(("<ip>", end));
    }

    let mut num_end = i;
    while num_end < b.len() && b[num_end].is_ascii_digit() {
        num_end += 1;
    }
    let token_end = word_end(b, num_end);
    if token_end > num_end {
        // Digits followed directly by letters: a hash, an id, or a unit.
        if let Some(placeholder) = classify(&b[i..token_end]) {
            return Some((placeholder, token_end));
        }
    } else if b.get(num_end) == Some(&b'.') && b.get(num_end + 1).is_some_and(u8::is_ascii_digit) {
        num_end += 1;
        while num_end < b.len() && b[num_end].is_ascii_digit() {
            num_end += 1;
        }
    }

    let unit = &b[num_end..word_end(b, num_end)];
    if unit.is_empty() {
        Some(("<num>", num_end))
    } else if is_duration_unit(unit) {
        Some(("<dur>", num_end + unit.len()))
    } else if unit.len() <= 3 && unit.iter().all(u8::is_ascii_alphabetic) {
        // Keep the unit as literal text: `512MB` becomes `<num>MB`.
        Some(("<num>", num_end))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(mask("retry 3 of 10"), "retry <num> of <num>");
        assert_eq!(mask("load 0.75 now"), "load <num> now");
        assert_eq!(mask("delta -5"), "delta -<num>");
        assert_eq!(mask("pid=4242 code=0"), "pid=<num> code=<num>");
        assert_eq!(mask("done."), "done.");
        assert_eq!(mask("took 5."), "took <num>.");
    }

    #[test]
    fn numbers_keep_short_units() {
        assert_eq!(mask("used 512MB of 2GB"), "used <num>MB of <num>GB");
        assert_eq!(mask("cpu 93.5% busy"), "cpu <num>% busy");
        assert_eq!(mask("status 5xx"), "status <num>xx");
    }

    #[test]
    fn words_with_digits_are_kept() {
        for word in ["http2", "sha256", "x86_64", "utf8", "oauth2", "ipv4", "v2"] {
            assert_eq!(mask(word), word);
        }
        assert_eq!(mask("3rd_party lib"), "3rd_party lib");
    }

    #[test]
    fn durations() {
        assert_eq!(mask("took 15ms"), "took <dur>");
        assert_eq!(mask("took 1.5s total"), "took <dur> total");
        assert_eq!(mask("lag=250µs"), "lag=<dur>");
        assert_eq!(mask("ttl 30m, grace 2h"), "ttl <dur>, grace <dur>");
        assert_eq!(mask("in 120sec"), "in <dur>");
        assert_eq!(mask("wait 900ns"), "wait <dur>");
    }

    #[test]
    fn hex() {
        assert_eq!(mask("at 0x7ffee4b2"), "at <hex>");
        assert_eq!(mask("commit 9f8e7d6c5b4a"), "commit <hex>");
        assert_eq!(mask("commit a1b2c3d4"), "commit <hex>");
        assert_eq!(mask("flags 0x1F"), "flags <hex>");
        // No digits: an ordinary word that happens to use hex letters.
        assert_eq!(mask("deadbeef facade"), "deadbeef facade");
        // Too short to be confident.
        assert_eq!(mask("tag abc123"), "tag abc123");
    }

    #[test]
    fn ids() {
        assert_eq!(mask("order ord_8f3k2 paid"), "order <id> paid");
        assert_eq!(mask("req=REQ12345X"), "req=<id>");
        assert_eq!(mask("session 1700abcxyz"), "session <id>");
    }

    #[test]
    fn uuids() {
        assert_eq!(
            mask("job 123e4567-e89b-12d3-a456-426614174000 done"),
            "job <uuid> done"
        );
        assert_eq!(
            mask("job=F47AC10B-58CC-4372-A567-0E02B2C3D479"),
            "job=<uuid>"
        );
        // Wrong group lengths: not a UUID.
        assert_eq!(mask("x 123e4567-e89b-12d3"), "x <hex>-e89b-12d3");
    }

    #[test]
    fn ipv4_addresses() {
        assert_eq!(mask("from 10.0.0.1"), "from <ip>");
        assert_eq!(mask("from 192.168.1.20:54321 ok"), "from <ip> ok");
        assert_eq!(mask("route 10.0.0.0/8 added"), "route <ip> added");
        assert_eq!(mask("peer 10.0.0.1: closed"), "peer <ip>: closed");
        assert_eq!(mask("bad 999.1.1.1"), "bad <num>.<num>");
        assert_eq!(mask("version 1.2.3"), "version <num>.<num>");
        assert_eq!(mask("build 1.2.3.4.5"), "build <num>.<num>.<num>");
    }

    #[test]
    fn ipv6_addresses() {
        assert_eq!(mask("from 2001:db8::1 ok"), "from <ip> ok");
        assert_eq!(mask("bind ::1"), "bind <ip>");
        assert_eq!(mask("from fe80::a00:27ff:fe4e:66a1"), "from <ip>");
        assert_eq!(mask("addr 2001:db8:0:0:0:0:0:1."), "addr <ip>.");
        // Clock times and Rust paths are not addresses.
        assert_eq!(mask("at 10:30:00"), "at <num>:<num>:<num>");
        assert_eq!(mask("std::io::Error"), "std::io::Error");
        assert_eq!(mask("de::Error"), "de::Error");
    }

    #[test]
    fn quoted_strings() {
        assert_eq!(mask(r#"user "alice" logged in"#), "user <str> logged in");
        assert_eq!(mask(r#"msg="a \"b\" c" n=1"#), "msg=<str> n=<num>");
        assert_eq!(mask("file 'a b.txt' missing"), "file <str> missing");
        assert_eq!(mask("said 'it's fine' twice"), "said <str> twice");
        assert_eq!(mask(r#"empty "" here"#), "empty <str> here");
    }

    #[test]
    fn apostrophes_and_unclosed_quotes_are_literal() {
        assert_eq!(mask("can't open, won't retry"), "can't open, won't retry");
        assert_eq!(mask(r#"unclosed "quote 7"#), r#"unclosed "quote <num>"#);
    }

    #[test]
    fn paths_with_ids() {
        assert_eq!(
            mask("GET /users/42/orders/9f8e7d6c"),
            "GET /users/<num>/orders/<hex>"
        );
        assert_eq!(
            mask("GET /v2/jobs/123e4567-e89b-12d3-a456-426614174000/logs"),
            "GET /v2/jobs/<uuid>/logs"
        );
        assert_eq!(
            mask("open /var/lib/app/7/data.db"),
            "open /var/lib/app/<num>/data.db"
        );
    }

    #[test]
    fn http_request_lines_keep_method_route_and_status() {
        assert_eq!(
            mask(r#"10.0.0.1 - bob "GET /users/42?full=1&x=y HTTP/1.1" 200 512 "-" "curl/8.1""#),
            r#"<ip> - bob "GET /users/<num>?<query> HTTP/<num>" 200 <num> <str> <str>"#
        );
        assert_eq!(
            mask(r#""POST /api/orders HTTP/2.0" 503 0"#),
            r#""POST /api/orders HTTP/<num>" 503 <num>"#
        );
        assert_eq!(
            mask(r#""DELETE /a?b" 1234"#),
            r#""DELETE /a?<query>" <num>"#
        );
        // Not a request line: an ordinary string.
        assert_eq!(mask(r#"said "GETTING there" 200"#), "said <str> <num>");
        assert_eq!(mask(r#"cut "GET /a HTTP/1.1"#), r#"cut "GET /a HTTP/<num>"#);
    }

    #[test]
    fn timestamps() {
        let line = "2024-01-15T10:30:00Z INFO synced until 2024-01-15 10:29:58.120";
        let mut out = String::new();
        mask_into(line, Some((0, 20)), &mut out);
        assert_eq!(out, "<ts> INFO synced until <ts>");

        let clf = "[15/Jan/2024:10:30:00 +0000] hit";
        mask_into(clf, Some((1, 27)), &mut out);
        assert_eq!(out, "[<ts>] hit");
    }

    #[test]
    fn similar_lines_share_a_template() {
        let a = mask("conn 17 from 10.0.0.5:4431 closed after 12.5ms (req a1b2c3d4)");
        let b = mask("conn 9 from 10.0.9.77:80 closed after 3s (req 00ff00ff00)");
        assert_eq!(a, b);
        assert_eq!(a, "conn <num> from <ip> closed after <dur> (req <hex>)");
    }

    #[test]
    fn non_ascii_text_is_preserved() {
        assert_eq!(mask("café 12 naïve"), "café <num> naïve");
        assert_eq!(mask("日本語 42"), "日本語 <num>");
        assert_eq!(mask("\u{fffd}\u{fffd} 7"), "\u{fffd}\u{fffd} <num>");
    }

    #[test]
    fn empty_and_plain() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("nothing to mask here"), "nothing to mask here");
    }
}
