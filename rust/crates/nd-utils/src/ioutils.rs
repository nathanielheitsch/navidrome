//! BOM-tolerant UTF-8 reading — port of `utils/ioutils`. Wraps a byte stream to
//! strip the UTF-8 BOM if present and convert UTF-16 (LE/BE) to UTF-8.

use std::io::{self, Read};

/// Wraps a reader to handle Byte Order Marks properly
/// (`ioutils.UTF8Reader`): strips the UTF-8 BOM if present, and converts
/// UTF-16 (LE/BE) to UTF-8. Useful for user-provided text files (LRC lyrics,
/// playlists) that may carry Windows BOM markers.
///
/// Reference: <https://en.wikipedia.org/wiki/Byte_order_mark>
#[must_use]
pub fn utf8_reader<R: Read>(mut r: R) -> impl Read {
    // Read enough for the BOM decision and convert up front: the Go transform
    // reader streams, but the ported consumers read whole files anyway.
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).ok();
    ByteReader::new(normalize_bom(buf))
}

/// A reader over already-normalized UTF-8 bytes.
struct ByteReader {
    buf: Vec<u8>,
    pos: usize,
}

impl ByteReader {
    fn new(buf: Vec<u8>) -> Self {
        Self { buf, pos: 0 }
    }
}

impl Read for ByteReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.buf.len() {
            return Ok(0);
        }
        let n = std::cmp::min(buf.len(), self.buf.len() - self.pos);
        buf[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Reads `filename` and returns its contents as UTF-8 bytes, automatically
/// handling BOM markers (`ioutils.UTF8ReadFile`): strips the UTF-8 BOM and
/// converts UTF-16-encoded files to UTF-8.
///
/// # Errors
/// IO errors from opening or reading the file.
pub fn utf8_read_file<P: AsRef<Path2>>(filename: P) -> io::Result<Vec<u8>> {
    let raw = std::fs::read(filename)?;
    Ok(normalize_bom(raw))
}

/// Alias so the doc can reference a path type without importing at the top.
#[allow(non_camel_case_types)]
type Path2 = std::path::Path;

/// Strips the UTF-8 BOM and converts UTF-16 to UTF-8.
#[must_use]
pub fn normalize_bom(raw: Vec<u8>) -> Vec<u8> {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return raw[3..].to_vec();
    }
    if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
        // UTF-16 LE
        return utf16_to_utf8(&raw[2..], true);
    }
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        // UTF-16 BE
        return utf16_to_utf8(&raw[2..], false);
    }
    raw
}

/// Converts UTF-16 code units (after the BOM) to UTF-8 bytes.
fn utf16_to_utf8(bytes: &[u8], little_endian: bool) -> Vec<u8> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            if little_endian {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    let s = String::from_utf16_lossy(&units);
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read_all(input: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        utf8_reader(&input[..]).read_to_end(&mut out).expect("read");
        out
    }

    #[test]
    fn strips_the_utf8_bom_marker() {
        let input = vec![0xEF, 0xBB, 0xBF, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(read_all(input), b"hello");
    }

    #[test]
    fn strips_utf8_bom_from_multiline_text() {
        let input: Vec<u8> = [0xEF, 0xBB, 0xBF]
            .into_iter()
            .chain(b"[00:00.00] test".iter().copied())
            .collect();
        assert_eq!(read_all(input), b"[00:00.00] test");
    }

    #[test]
    fn passes_through_text_without_bom() {
        assert_eq!(read_all(b"hello world".to_vec()), b"hello world");
    }

    #[test]
    fn converts_utf16_le_to_utf8() {
        let input = vec![0xFF, 0xFE, b'h', 0x00, b'i', 0x00];
        assert_eq!(read_all(input), b"hi");
    }

    #[test]
    fn converts_utf16_be_to_utf8() {
        let input = vec![0xFE, 0xFF, 0x00, b'h', 0x00, b'i'];
        assert_eq!(read_all(input), b"hi");
    }

    #[test]
    fn returns_empty_for_empty_content() {
        assert_eq!(read_all(Vec::new()), b"");
    }

    #[test]
    fn utf8_read_file_strips_the_bom() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("bom-test.lrc");
        std::fs::write(
            &path,
            [0xEF, 0xBB, 0xBF]
                .into_iter()
                .chain(b"[00:00.00] hello".iter().copied())
                .collect::<Vec<u8>>(),
        )
        .expect("write");
        let contents = utf8_read_file(&path).expect("read");
        assert_ne!(contents[0], 0xEF);
        assert_eq!(contents[0], b'[');
        assert!(contents.starts_with(b"[00:00.00]"));
    }

    #[test]
    fn utf8_read_file_reads_normally_without_bom() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.lrc");
        std::fs::write(&path, "[00:18.80]We're no strangers to love").expect("write");
        let contents = utf8_read_file(&path).expect("read");
        let s = String::from_utf8(contents).expect("utf8");
        assert!(s.contains("We're no strangers to love"));
    }

    #[test]
    fn utf8_read_file_errors_on_missing_file() {
        assert!(utf8_read_file("/nonexistent/dir/file.lrc").is_err());
    }
}
