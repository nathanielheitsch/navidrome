//! Minimal port of `github.com/google/uuid` parsing/canonicalization, which
//! `model/metadata/metadata.go:403-411` uses to normalize UUID-typed tags.
//!
//! Only the shapes Go's `uuid.Parse` accepts are supported (`uuid.go:64-114`):
//! 36-char canonical, `urn:uuid:` prefixed, `{...}` wrapped, and 32 bare hex
//! digits. Anything else is an error, and a tag whose value fails to parse is
//! dropped by `sanitize` rather than persisted.

/// A parsed UUID (Go `uuid.UUID`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// `uuid.UUID.String()` (`uuid.go:190-206`): lowercase canonical form with
    /// dashes — the form Go stores in the database.
    #[must_use]
    pub fn canonical(&self) -> String {
        let b = &self.0;
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
        )
    }
}

impl std::fmt::Display for Uuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Errors from [`parse`] (Go's `uuid.Parse` error values; the caller only
/// distinguishes success from failure, as `sanitize` does).
#[derive(Debug, thiserror::Error)]
pub enum UuidError {
    /// The string length is not one of the accepted shapes.
    #[error("invalid UUID length: {0}")]
    InvalidLength(usize),
    /// The shape matched but the bytes are not a UUID.
    #[error("invalid UUID format")]
    InvalidFormat,
}

/// Byte value of a hex digit (Go `xtob`, which accepts `0-9`, `a-f`, `A-F`).
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `uuid.Parse` (`uuid.go:64-114`).
///
/// # Errors
///
/// [`UuidError`] when the value is not one of Go's accepted shapes.
pub fn parse(s: &str) -> Result<Uuid, UuidError> {
    let bytes = s.as_bytes();
    let body: &[u8] = match bytes.len() {
        36 => bytes,
        // `urn:uuid:...` — the prefix check is case-insensitive in Go.
        45 => {
            if !bytes[..9].eq_ignore_ascii_case(b"urn:uuid:") {
                return Err(UuidError::InvalidFormat);
            }
            &bytes[9..]
        }
        // `{...}` — Go drops the leading brace and requires the trailing one.
        38 => {
            if bytes[0] != b'{' || bytes[37] != b'}' {
                return Err(UuidError::InvalidFormat);
            }
            &bytes[1..37]
        }
        // 32 bare hex digits, parsed before the dash checks.
        32 => {
            let mut out = [0u8; 16];
            for i in 0..16 {
                let hi = hex_val(bytes[i * 2]).ok_or(UuidError::InvalidFormat)?;
                let lo = hex_val(bytes[i * 2 + 1]).ok_or(UuidError::InvalidFormat)?;
                out[i] = hi << 4 | lo;
            }
            return Ok(Uuid(out));
        }
        other => return Err(UuidError::InvalidLength(other)),
    };

    // From here the body must be the dashed form.
    if body.len() != 36
        || body[8] != b'-'
        || body[13] != b'-'
        || body[18] != b'-'
        || body[23] != b'-'
    {
        return Err(UuidError::InvalidFormat);
    }
    let positions = [
        0usize, 2, 4, 6, 9, 11, 14, 16, 19, 21, 24, 26, 28, 30, 32, 34,
    ];
    let mut out = [0u8; 16];
    for (i, p) in positions.iter().enumerate() {
        let hi = hex_val(body[*p]).ok_or(UuidError::InvalidFormat)?;
        let lo = hex_val(body[*p + 1]).ok_or(UuidError::InvalidFormat)?;
        out[i] = hi << 4 | lo;
    }
    Ok(Uuid(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from model/metadata/metadata_test.go:156-167 (UUID normalization).
    #[test]
    fn accepts_go_shapes_and_canonicalizes() {
        let want = "f81d4fae-7dec-11d0-a765-00a0c91e6bf6";
        for raw in [
            "f81d4fae-7dec-11d0-a765-00a0c91e6bf6",
            "F81D4FAE-7DEC-11D0-A765-00A0C91E6BF6",
            "{f81d4fae-7dec-11d0-a765-00a0c91e6bf6}",
            "urn:uuid:f81d4fae-7dec-11d0-a765-00a0c91e6bf6",
            "URN:UUID:f81d4fae-7dec-11d0-a765-00a0c91e6bf6",
            "f81d4fae7dec11d0a76500a0c91e6bf6",
        ] {
            assert_eq!(parse(raw).unwrap().canonical(), want, "raw {raw}");
        }
    }

    #[test]
    fn rejects_non_uuid() {
        assert!(parse("the beatles").is_err());
        assert!(parse("").is_err());
        assert!(parse("f81d4fae-7dec-11d0-a765-00a0c91e6bf").is_err());
        assert!(parse("f81d4fae-7dec-11d0-a765-00a0c91e6bfg").is_err());
        assert!(parse("{f81d4fae-7dec-11d0-a765-00a0c91e6bf6").is_err());
        assert!(parse("urn:notuuid:f81d4fae-7dec-11d0-a765").is_err());
    }
}
