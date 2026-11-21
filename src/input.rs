//! Input handling: opening files or stdin, transparent gzip, and a line
//! reader with a hard cap on line length.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};

use flate2::bufread::MultiGzDecoder;

/// Bytes kept from one line. The rest of a longer line is read and dropped,
/// so a file without newlines cannot exhaust memory.
pub const MAX_LINE_BYTES: usize = 1 << 20;

const BUFFER_BYTES: usize = 1 << 18;

/// Opens `path` for reading, where `-` means stdin. Input that starts with
/// the gzip magic bytes is decompressed.
pub fn open(path: &str) -> io::Result<Box<dyn BufRead>> {
    let raw: Box<dyn Read> = if path == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(path)?)
    };
    let mut reader = BufReader::with_capacity(BUFFER_BYTES, raw);
    if reader.fill_buf()?.starts_with(&[0x1f, 0x8b]) {
        let gz = MultiGzDecoder::new(reader);
        Ok(Box::new(BufReader::with_capacity(BUFFER_BYTES, gz)))
    } else {
        Ok(Box::new(reader))
    }
}

/// What [`LineReader::read_line`] learned about the line it returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineInfo {
    /// Bytes consumed from the input, including the line terminator.
    pub bytes: u64,
    /// Whether the line was longer than the limit and has been cut.
    pub truncated: bool,
}

pub struct LineReader<R> {
    inner: R,
    max_line: usize,
}

impl<R: BufRead> LineReader<R> {
    pub fn new(inner: R) -> Self {
        Self::with_max_line(inner, MAX_LINE_BYTES)
    }

    pub fn with_max_line(inner: R, max_line: usize) -> Self {
        LineReader { inner, max_line }
    }

    /// Reads the next line into `buf` without its `\n` or `\r\n`. Returns
    /// `None` at end of input.
    pub fn read_line(&mut self, buf: &mut Vec<u8>) -> io::Result<Option<LineInfo>> {
        buf.clear();
        let mut info = LineInfo {
            bytes: 0,
            truncated: false,
        };
        loop {
            let available = match self.inner.fill_buf() {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                if info.bytes == 0 {
                    return Ok(None);
                }
                break;
            }
            let (chunk, used, done) = match memchr::memchr(b'\n', available) {
                Some(pos) => (&available[..pos], pos + 1, true),
                None => (available, available.len(), false),
            };
            let room = self.max_line - buf.len();
            if chunk.len() > room {
                info.truncated = true;
            }
            buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
            info.bytes += used as u64;
            self.inner.consume(used);
            if done {
                break;
            }
        }
        if !info.truncated && buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(Some(info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8], capacity: usize, max_line: usize) -> Vec<(Vec<u8>, LineInfo)> {
        let reader = BufReader::with_capacity(capacity, input);
        let mut reader = LineReader::with_max_line(reader, max_line);
        let mut buf = Vec::new();
        let mut out = Vec::new();
        while let Some(info) = reader.read_line(&mut buf).unwrap() {
            out.push((buf.clone(), info));
        }
        out
    }

    fn texts(input: &[u8], capacity: usize) -> Vec<String> {
        lines(input, capacity, 1024)
            .into_iter()
            .map(|(l, _)| String::from_utf8(l).unwrap())
            .collect()
    }

    #[test]
    fn splits_on_newlines_across_buffer_refills() {
        for capacity in [1, 2, 3, 7, 64] {
            assert_eq!(
                texts(b"one\ntwo\r\n\nlast", capacity),
                ["one", "two", "", "last"],
                "capacity {capacity}"
            );
        }
    }

    #[test]
    fn trailing_newline_does_not_add_a_line() {
        assert_eq!(texts(b"a\nb\n", 4), ["a", "b"]);
        assert!(texts(b"", 4).is_empty());
        assert_eq!(texts(b"\n", 4), [""]);
    }

    #[test]
    fn counts_bytes_including_terminators() {
        let all = lines(b"ab\r\ncd\nef", 3, 1024);
        let bytes: Vec<u64> = all.iter().map(|(_, i)| i.bytes).collect();
        assert_eq!(bytes, [4, 3, 2]);
    }

    #[test]
    fn long_lines_are_cut_and_the_rest_skipped() {
        let all = lines(b"0123456789abcdef\nok\n", 4, 6);
        assert_eq!(all[0].0, b"012345");
        assert!(all[0].1.truncated);
        assert_eq!(all[0].1.bytes, 17);
        assert_eq!(all[1].0, b"ok");
        assert!(!all[1].1.truncated);
    }

    #[test]
    fn keeps_arbitrary_bytes() {
        let all = lines(b"\xff\x00\xfe\nx", 8, 1024);
        assert_eq!(all[0].0, b"\xff\x00\xfe");
    }
}
