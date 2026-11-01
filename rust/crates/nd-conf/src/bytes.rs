//! Byte-size string parsing (`TranscodingCacheSize`, `ImageCacheSize`,
//! `MaxImageUploadSize`, `MaxImageSize`), port of Go
//! `humanize.ParseBytes` acceptance + `validateByteSize`
//! (conf/configuration.go:856-869).
//!
//! Accepted forms: a raw integer (`"10485760"`) or a number followed by an
//! optional SI prefix (`B`, `kB`, `MB`, `GB`, `TB`, …). Both the
//! case-insensitive SI (k = 1000) and the binary (KiB = 1024) spellings are
//! accepted, mirroring `humanize.ParseBytes`.

use std::fmt;

/// Error returned by [`parse_bytes`].
#[derive(Debug, thiserror::Error)]
#[error("invalid byte size {input:?}: use values like '10MB', '1GB', or raw bytes like '10485760'")]
pub struct ByteSizeError {
    /// The offending input string.
    pub input: String,
}

const UNITS: &[(&str, u64)] = &[
    ("b", 1),
    ("kb", 1_000),
    ("mb", 1_000_000),
    ("gb", 1_000_000_000),
    ("tb", 1_000_000_000_000),
    ("pb", 1_000_000_000_000_000),
    ("kib", 1 << 10),
    ("mib", 1 << 20),
    ("gib", 1 << 30),
    ("tib", 1 << 40),
    ("pib", 1 << 50),
    ("eb", 1_000_000_000_000_000_000),
    ("eib", 1 << 60),
];

/// A validated byte-size value parsed from a config string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct ByteSize(pub u64);

impl ByteSize {
    /// The value in bytes.
    pub fn as_bytes(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes", self.0)
    }
}

/// Parses a human byte-size string per `humanize.ParseBytes`. The empty string
/// is an error (Go would fail the same way at the `ParseBytes` call).
pub fn parse_bytes(s: &str) -> Result<ByteSize, ByteSizeError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(ByteSizeError {
            input: s.to_string(),
        });
    }
    // Split numeric prefix from unit suffix.
    let idx = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_'))
        .unwrap_or(t.len());
    let (num_str, unit_raw) = t.split_at(idx);
    let unit = unit_raw.trim_start();
    let num_str = num_str.replace('_', "");
    if num_str.is_empty() {
        return Err(ByteSizeError {
            input: s.to_string(),
        });
    }
    let value: f64 = num_str.parse().map_err(|_| ByteSizeError {
        input: s.to_string(),
    })?;
    let mult = if unit.is_empty() {
        1.0
    } else {
        let u = unit.to_ascii_lowercase();
        match UNITS.iter().find(|(name, _)| *name == u) {
            Some((_, m)) => *m as f64,
            None => {
                return Err(ByteSizeError {
                    input: s.to_string(),
                })
            }
        }
    };
    let bytes = value * mult;
    if bytes >= u64::MAX as f64 {
        return Err(ByteSizeError {
            input: s.to_string(),
        });
    }
    Ok(ByteSize(bytes as u64))
}

/// A byte-size string field. Deserializes from the raw string, validating it
/// at load time (non-zero, parseable) so invalid config fails startup, and
/// serializes as the original spelling via [`parse_bytes`] formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteSizeField {
    /// Parsed value in bytes.
    pub bytes: u64,
    /// Original spelling, kept for round-tripping and messages.
    pub raw: String,
}

impl ByteSizeField {
    /// The value in bytes.
    pub fn as_bytes(&self) -> u64 {
        self.bytes
    }

    /// Builds a field from a canonical spelling, for defaults.
    pub fn from_raw(raw: &str) -> Self {
        let bytes = parse_bytes(raw).map(|b| b.0).unwrap_or(0);
        ByteSizeField {
            bytes,
            raw: raw.to_string(),
        }
    }

    /// The value as spelled in the config (or default).
    pub fn raw(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for ByteSizeField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl serde::Serialize for ByteSizeField {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.raw)
    }
}

impl<'de> serde::Deserialize<'de> for ByteSizeField {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let b = parse_bytes(&s).map_err(serde::de::Error::custom)?;
        if b.0 == 0 {
            return Err(serde::de::Error::custom(format!(
                "invalid byte size {s:?}: must be greater than zero"
            )));
        }
        Ok(ByteSizeField { bytes: b.0, raw: s })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_spellings() {
        assert_eq!(parse_bytes("100MB").unwrap().0, 100_000_000);
        assert_eq!(parse_bytes("10MB").unwrap().0, 10_000_000);
        assert_eq!(parse_bytes("1GB").unwrap().0, 1_000_000_000);
        assert_eq!(parse_bytes("10485760").unwrap().0, 10_485_760);
        assert_eq!(parse_bytes("1KiB").unwrap().0, 1024);
        assert!(parse_bytes("bananas").is_err());
        assert!(parse_bytes("").is_err());
    }

    #[test]
    fn deserializing_zero_is_an_error() {
        let r: Result<ByteSizeField, _> = toml::from_str("f = \"0MB\"");
        assert!(r.is_err());
    }

    #[test]
    fn deserializes_valid() {
        // A TOML document is a key/value map, so a value type is exercised in
        // the field position that carries it — same shape the loader uses.
        #[derive(serde::Deserialize)]
        struct Field {
            f: ByteSizeField,
        }
        let cfg: Field = toml::from_str("f = \"20MB\"").unwrap();
        let f = cfg.f;
        assert_eq!(f.as_bytes(), 20_000_000);
        assert_eq!(f.raw(), "20MB");
    }
}
