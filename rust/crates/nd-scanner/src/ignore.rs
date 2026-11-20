//! `.ndignore` policy — a **pinned translation** of the Go matcher, not a generic
//! gitignore crate.
//!
//! Sources, mirrored line by line:
//! - `scanner/ignore_checker.go:35-101` (`Push`/`Pop`/`PushAllParents`/`ShouldIgnore`).
//! - `scanner/ignore_checker.go:106-145` (`loadPatternsFromFolder`).
//! - `scanner/ignore_checker.go:148-163` (`rebuildCurrentPatterns`/`compilePatterns`).
//! - `github.com/sabhiram/go-gitignore` **v0.0.0-20210923224102-525f6e181f06**
//!   (`go.mod:52`, `go.sum:231-232`): `github.com/sabhiram/go-gitignore/ignore.go:73-147` (`getPatternFromLine`) and
//!   `github.com/sabhiram/go-gitignore/ignore.go:217-237` (`MatchesPathHow`).
//!
//! ## Why a hand translation
//!
//! The pinned matcher is *not* normal gitignore, so an `ignore`/`globset` crate would
//! change behaviour:
//! * the pattern stack is flattened **without rebasing** each level's patterns, so a
//!   root pattern keeps matching library-relative paths at every depth;
//! * `?` is a **literal** character, not a single-character wildcard (`?.mp3` matches
//!   nothing, `a?b` matches only the literal name `a?b`);
//! * a trailing slash only produces a `(|.*)$`/`(|/.*)$` suffix, so `build/` does not
//!   match the bare name `build` but does match `build/out.mp3`;
//! * uncompileable regexes are **silently omitted** by Go;
//! * negation is applied in list order, so `!a.txt` before `*.txt` is a no-op while
//!   `*.txt` then `!a.txt` re-includes `a.txt`.
//!
//! ## Declared departures (all reported, none silent)
//!
//! * A pattern Go cannot compile is omitted silently; here it yields
//!   [`ScanWarningKind::IgnorePatternInvalid`]. Same effective behaviour (no pattern),
//!   better observability.
//! * Brace, `(?…)`, and Perl class/boundary escape syntax is conservatively refused with
//!   [`ScanWarningKind::IgnorePatternUnsupported`]. This is broader than demonstrated
//!   engine differences: valid repetitions and translated literal question-mark groups
//!   can agree. Go accepts malformed braces as literals where Rust may reject them.
//!   Refusal can change matching even where translation would agree; Go evidence is
//!   recorded in `tests/ignore_policy.rs`.
//! * Lines are required to be valid UTF-8 (Go works on bytes). A non-UTF-8 line ends
//!   the read like a Go read failure does: the patterns accepted before it are kept.
//! * For ordinary local-file reads, Scanner's 65,536-byte buffer ceiling counts raw
//!   terminators: max payload LF M-1, CRLF M-2, EOF M-1 (before CR stripping).
//!   No parity claim is made for arbitrary Reader `(n, err)` combinations.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use regex::Regex;

use crate::contract::{ScanWarning, ScanWarningKind, SCAN_IGNORE_FILE};
use crate::storage::LogicalPath;

/// Ordinary local-file Scanner byte-buffer ceiling, including raw terminator bytes.
pub const MAX_IGNORE_LINE_BYTES: usize = 64 * 1024;

/// Placeholder Go uses for escaped stars (`github.com/sabhiram/go-gitignore/ignore.go:113`), restored afterwards.
const MAGIC_STAR: &str = "#$~";

/// How one `.ndignore` file contributed to the pattern stack. The distinction matters:
/// a *stat* failure contributes nothing silently, while a *read* failure keeps the
/// patterns already accumulated and must never become "ignore everything".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreFileOutcome {
    /// No file, or its stat failed: no patterns, no warning (`scanner/ignore_checker.go:111-114`).
    Absent,
    /// Patterns from a file read to completion. An empty or comment-only file yields
    /// exactly `["**/*"]` (`scanner/ignore_checker.go:138-142`) — ignore everything.
    Loaded(Vec<String>),
    /// The file existed but could not be opened: no patterns, one warning
    /// (`scanner/ignore_checker.go:117-121`).
    OpenFailed,
    /// Reading stopped partway: the patterns accepted **before** the failure are kept,
    /// plus a warning. Never an ignore-all (`scanner/ignore_checker.go:133-136`).
    ReadFailedPrefix {
        /// Lines accepted before the failure, already trimmed and filtered.
        patterns: Vec<String>,
        /// Which failure ended the read.
        kind: ScanWarningKind,
    },
}

impl IgnoreFileOutcome {
    /// Patterns this file contributes to the stack.
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        match self {
            IgnoreFileOutcome::Absent | IgnoreFileOutcome::OpenFailed => &[],
            IgnoreFileOutcome::Loaded(patterns) => patterns,
            IgnoreFileOutcome::ReadFailedPrefix { patterns, .. } => patterns,
        }
    }

    /// Warnings this outcome carries, attributed to a library and folder path.
    #[must_use]
    pub fn warnings(&self, library_id: i64, folder: LogicalPath) -> Vec<ScanWarning> {
        match self {
            IgnoreFileOutcome::OpenFailed => vec![ScanWarning {
                kind: ScanWarningKind::IgnoreFileOpenFailed,
                library_id: Some(library_id),
                path: Some(folder),
                message: format!("cannot open {SCAN_IGNORE_FILE}; no patterns from it"),
            }],
            IgnoreFileOutcome::ReadFailedPrefix { kind, patterns } => vec![ScanWarning {
                kind: *kind,
                library_id: Some(library_id),
                path: Some(folder),
                message: format!(
                    "{SCAN_IGNORE_FILE} read failed; kept {} pattern(s) accepted before the failure",
                    patterns.len()
                ),
            }],
            IgnoreFileOutcome::Absent | IgnoreFileOutcome::Loaded(_) => Vec::new(),
        }
    }
}

/// One translated pattern line: the compiled matcher plus Go's negation flag.
#[derive(Debug, Clone)]
pub struct Pattern {
    /// The line as parsed (after trimming), for diagnostics.
    pub line: String,
    /// Whether the line began with `!` (`github.com/sabhiram/go-gitignore/ignore.go:93-97`).
    pub negate: bool,
    regex: Regex,
}

impl PartialEq for Pattern {
    /// Structural comparison of the translated expression and negation flag, so tests can
    /// assert translation results without exposing the compiled regex.
    fn eq(&self, other: &Pattern) -> bool {
        self.line == other.line
            && self.negate == other.negate
            && self.regex.as_str() == other.regex.as_str()
    }
}

impl Eq for Pattern {}

impl Pattern {
    /// The translated expression, for golden comparison against Go's compiled pattern.
    #[must_use]
    pub fn expression(&self) -> &str {
        self.regex.as_str()
    }

    /// Whether this pattern matches a library-relative slash path.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        self.regex.is_match(path)
    }
}

/// Result of translating one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Translation {
    /// A usable pattern.
    Compiled(Pattern),
    /// Nothing to compile: empty, comment, or whitespace-only.
    Skipped,
    /// The translated expression failed to compile; the pattern is omitted.
    Invalid {
        /// Line text.
        line: String,
        /// Translated expression (diagnostics only; never compiled).
        expr: String,
        /// Rust's parse error.
        message: String,
    },
    /// Syntax conservatively refused by policy, including some engine-equivalent cases.
    Unsupported {
        /// Line text.
        line: String,
        /// Which policy-refused syntax was observed.
        construct: &'static str,
    },
}

impl Translation {
    /// Display form of the pattern text this translation is about.
    #[must_use]
    pub fn line(&self) -> &str {
        match self {
            Translation::Compiled(pattern) => &pattern.line,
            Translation::Skipped => "",
            Translation::Invalid { line, .. } | Translation::Unsupported { line, .. } => line,
        }
    }
}

/// Translate one `.ndignore` line, mirroring `github.com/sabhiram/go-gitignore/ignore.go:73-147` (`getPatternFromLine`)
/// step for step and in the same order.
#[must_use]
pub fn translate_line(line: &str) -> Translation {
    // TrimRight(line, "\r"): every trailing CR, not just one.
    let mut text: String = line.chars().collect();
    while text.ends_with('\r') {
        text.pop();
    }

    // [Rule 2] comment.
    if text.starts_with('#') {
        return Translation::Skipped;
    }

    // [Rule 3] `strings.Trim(line, " ")`: ASCII spaces on **both** ends. The caller has
    // already applied Unicode TrimSpace (`scanner/ignore_checker.go:126`), so tabs are gone.
    text = text.trim_matches(' ').to_string();
    if text.is_empty() {
        return Translation::Skipped;
    }

    // [Rule 4] negation prefix.
    let mut negate = false;
    if let Some(rest) = text.strip_prefix('!') {
        negate = true;
        text = rest.to_string();
    }

    // Escaped `#`/`!` at the start (`github.com/sabhiram/go-gitignore/ignore.go:99-103`): the backslash is dropped here
    // and remains in the expression as a punctuation escape, which both engines accept.
    if text.starts_with('#') || text.starts_with('!') {
        text = text[1..].to_string();
    }

    // A `foo/*.blah`-shaped line found in a folder gets a leading `/` (`github.com/sabhiram/go-gitignore/ignore.go:106-107`),
    // i.e. `regexp.MustCompile(`([^\/+])/.*\*\.`)` must match and the line must not
    // already start with `/`.
    if glob_needs_anchor(&text) && !text.starts_with('/') {
        text.insert(0, '/');
    }

    // Refuse constructs the engines disagree on *before* translating anything, so an
    // unsupported pattern is never quietly reinterpreted.
    if let Some(construct) = unsupported_construct(&text) {
        return Translation::Unsupported {
            line: line.to_string(),
            construct,
        };
    }

    // Escape dots, then the `**` forms, then escaped stars, stars, question marks —
    // exactly Go's order, since the order changes the result.
    text = replace_all(&text, ".", r"\.");
    if text.starts_with("/**/") {
        text = text[1..].to_string();
    }
    text = replace_all(&text, "/**/", r"(/|/.+/)");
    text = replace_all(&text, "**/", &format!("(|.{MAGIC_STAR}/)"));
    text = replace_all(&text, "/**", &format!("(|/.{MAGIC_STAR})"));
    text = replace_all(&text, r"\*", &format!(r"\{MAGIC_STAR}"));
    text = replace_all(&text, "*", "([^/]*)");
    text = replace_all(&text, "?", r"\?");
    text = replace_all(&text, MAGIC_STAR, "*");

    let mut expr = String::new();
    if text.ends_with('/') {
        expr.push_str(&text);
        expr.push_str("(|.*)$");
    } else {
        expr.push_str(&text);
        expr.push_str("(|/.*)$");
    }
    if expr.starts_with('/') {
        expr = format!("^(|/){}", &expr[1..]);
    } else {
        expr = format!("^(|.*/){expr}");
    }

    match Regex::new(&expr) {
        Ok(regex) => Translation::Compiled(Pattern {
            line: line.to_string(),
            negate,
            regex,
        }),
        Err(err) => Translation::Invalid {
            line: line.to_string(),
            expr,
            message: err.to_string(),
        },
    }
}

/// `regexp.MustCompile(`([^\/+])/.*\*\.`).MatchString(line)`: some character that is
/// neither `/` nor `+`, then `/`, then anything, then a literal `*` immediately
/// followed by `.`.
fn glob_needs_anchor(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'/' || *byte == b'+' {
            continue;
        }
        if index + 1 >= bytes.len() || bytes[index + 1] != b'/' {
            continue;
        }
        if bytes[index + 2..].windows(2).any(|pair| pair == b"*.") {
            return true;
        }
    }
    false
}

/// Conservative refusal policy, not a claim that every such expression differs.
/// Valid repetitions and translated literal question-mark groups can agree; malformed
/// braces may be literal in Go but invalid in Rust. Perl classes/boundaries have ASCII
/// semantics in Go and Unicode defaults here. Refuse these whole syntax families.
fn unsupported_construct(line: &str) -> Option<&'static str> {
    if line.contains('{') || line.contains('}') {
        return Some("brace syntax (conservatively refused by policy)");
    }
    if line.contains("(?") {
        return Some("(?…) syntax (conservatively refused by policy)");
    }
    if line.as_bytes().windows(2).any(|pair| {
        pair[0] == b'\\'
            && matches!(
                pair[1],
                b'd' | b'D' | b'w' | b'W' | b's' | b'S' | b'b' | b'B'
            )
    }) {
        return Some("Perl class/boundary escape (conservatively refused by policy)");
    }
    None
}

/// `strings.Replace(…, -1)` / `ReplaceAllString`: leftmost, non-overlapping.
fn replace_all(haystack: &str, needle: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(index) = rest.find(needle) {
        out.push_str(&rest[..index]);
        out.push_str(replacement);
        rest = &rest[index + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// The stack-based checker: push on entry, pop on exit, exactly like `IgnoreChecker`.
///
/// Frames hold *compiled* patterns; matching flattens the frames in push order and
/// applies negations in list order, which is what Go's `MatchesPathHow` does over the
/// flattened pattern list.
#[derive(Debug, Default, Clone)]
pub struct IgnoreStack {
    frames: Vec<Vec<Pattern>>,
    warnings: Vec<ScanWarning>,
}

impl IgnoreStack {
    /// An empty stack: nothing is ignored.
    #[must_use]
    pub fn new() -> IgnoreStack {
        IgnoreStack {
            frames: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Frames currently pushed (root first).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Compiled patterns in the current frame, for tests and diagnostics.
    #[must_use]
    pub fn current_patterns(&self) -> &[Pattern] {
        self.frames.last().map(Vec::as_slice).unwrap_or(&[])
    }

    /// Warnings accumulated while loading and compiling.
    #[must_use]
    pub fn warnings(&self) -> &[ScanWarning] {
        &self.warnings
    }

    /// Take the accumulated warnings.
    pub fn take_warnings(&mut self) -> Vec<ScanWarning> {
        std::mem::take(&mut self.warnings)
    }

    /// `scanner/ignore_checker.go:36-39` — push one folder level's patterns.
    pub fn push(&mut self, patterns: &[String]) {
        let mut frame = Vec::with_capacity(patterns.len());
        for line in patterns {
            match translate_line(line) {
                Translation::Compiled(pattern) => frame.push(pattern),
                Translation::Skipped => {}
                Translation::Invalid { expr, message, .. } => {
                    self.warnings.push(ScanWarning {
                        kind: ScanWarningKind::IgnorePatternInvalid,
                        library_id: None,
                        path: None,
                        message: format!(
                            "pattern {line:?} produced invalid regex ({expr}: {message}); omitted"
                        ),
                    });
                }
                Translation::Unsupported { construct, .. } => {
                    self.warnings.push(ScanWarning {
                        kind: ScanWarningKind::IgnorePatternUnsupported,
                        library_id: None,
                        path: None,
                        message: format!(
                            "pattern {line:?} uses {construct}; not translated rather than reinterpreted"
                        ),
                    });
                }
            }
        }
        self.frames.push(frame);
    }

    /// `scanner/ignore_checker.go:45-48` — pop the most recent level; an empty stack is a no-op.
    pub fn pop(&mut self) {
        self.frames.pop();
    }

    /// `scanner/ignore_checker.go:56-81` — push the root, then every ancestor prefix of
    /// `target`. `load` is the caller's `.ndignore` reader, so this works over any
    /// filesystem seam; it receives the folder's ancestor component names.
    pub fn push_all_parents<L, F>(&mut self, target: L, mut load: F)
    where
        L: AsRef<OsStr>,
        F: FnMut(&[&OsStr]) -> IgnoreFileOutcome,
    {
        let target = target.as_ref();
        self.record(load(&[]));
        if target.is_empty() || target == OsStr::new(".") {
            return;
        }
        // Go splits `path.Clean(targetPath)` on "/" and skips `.`/empty parts. Lossy
        // text is fine here: the components are re-used as OsStr for I/O by the loader,
        // and a non-UTF-8 ancestor name is reported by the walker separately.
        let mut ancestors: Vec<&OsStr> = Vec::new();
        for part in target.to_string_lossy().split('/') {
            if part == "." || part.is_empty() {
                continue;
            }
            ancestors.push(OsStr::new(part));
            self.record(load(&ancestors));
        }
    }

    fn record(&mut self, outcome: IgnoreFileOutcome) {
        match &outcome {
            IgnoreFileOutcome::OpenFailed => self.warnings.push(ScanWarning {
                kind: ScanWarningKind::IgnoreFileOpenFailed,
                library_id: None,
                path: None,
                message: format!("cannot open {SCAN_IGNORE_FILE}; no patterns from it"),
            }),
            IgnoreFileOutcome::ReadFailedPrefix { kind, patterns } => self.warnings.push(
                ScanWarning {
                    kind: *kind,
                    library_id: None,
                    path: None,
                    message: format!(
                        "{SCAN_IGNORE_FILE} read failed; kept {} pattern(s) accepted before the failure",
                        patterns.len()
                    ),
                },
            ),
            IgnoreFileOutcome::Absent | IgnoreFileOutcome::Loaded(_) => {}
        }
        let patterns = outcome.patterns().to_vec();
        self.push(&patterns);
    }

    /// `scanner/ignore_checker.go:85-101` — the root and an empty path are never ignored, and an
    /// empty stack ignores nothing.
    #[must_use]
    pub fn should_ignore(&self, path: &str) -> bool {
        if path.is_empty() || path == "." {
            return false;
        }
        let mut ignored = false;
        for frame in &self.frames {
            for pattern in frame {
                if pattern.matches(path) {
                    ignored = !pattern.negate;
                }
            }
        }
        ignored
    }
}

/// Read one `.ndignore` file with Go's failure taxonomy.
///
/// `path` is the OS path of `<folder>/.ndignore`. A stat failure is silent and yields
/// [`IgnoreFileOutcome::Absent`], matching Go's `fs.Stat` check.
pub fn read_ignore_file(path: &Path) -> IgnoreFileOutcome {
    if std::fs::metadata(path).is_err() {
        return IgnoreFileOutcome::Absent;
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return IgnoreFileOutcome::OpenFailed,
    };
    read_ignore_reader(file)
}

/// Emulate Scanner for ordinary local-file reads, including a final partial token
/// before a read error. Not a promise for arbitrary Go Reader `(n, err)` combinations.
pub fn read_ignore_reader(reader: impl Read) -> IgnoreFileOutcome {
    let mut reader = BufReader::new(reader);
    let mut patterns: Vec<String> = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();

    loop {
        buffer.clear();
        let failure = match read_line(&mut reader, &mut buffer) {
            LineRead::Eof => break,
            LineRead::Line => None,
            LineRead::Failed(ScanWarningKind::IgnoreFileReadFailed) => {
                Some(ScanWarningKind::IgnoreFileReadFailed)
            }
            LineRead::Failed(kind) => {
                return IgnoreFileOutcome::ReadFailedPrefix { patterns, kind };
            }
        };
        // ScanLines drops the trailing LF; drop one trailing CR too, so a CRLF line
        // reaches `translate_line` like Go's line text does.
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }
        let text = match std::str::from_utf8(&buffer) {
            Ok(text) => text,
            Err(_) => {
                return IgnoreFileOutcome::ReadFailedPrefix {
                    patterns,
                    kind: ScanWarningKind::IgnoreFileNotUtf8,
                };
            }
        };
        // `strings.TrimSpace` — Unicode whitespace, both ends.
        let trimmed = text.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            patterns.push(trimmed.to_string());
        }
        if let Some(kind) = failure {
            return IgnoreFileOutcome::ReadFailedPrefix { patterns, kind };
        }
    }

    if patterns.is_empty() {
        // Go: an empty or comment-only file means "ignore everything".
        return IgnoreFileOutcome::Loaded(vec![String::from("**/*")]);
    }
    IgnoreFileOutcome::Loaded(patterns)
}

/// Outcome of one scanner line, mirroring `bufio.Scanner`'s states.
enum LineRead {
    /// EOF with nothing pending.
    Eof,
    /// One line accepted (newline stripped).
    Line,
    /// Read failure or an over-long token.
    Failed(ScanWarningKind),
}

fn read_line(reader: &mut BufReader<impl Read>, out: &mut Vec<u8>) -> LineRead {
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                return if out.is_empty() {
                    LineRead::Eof
                } else {
                    LineRead::Line
                };
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    return LineRead::Line;
                }
                out.push(byte[0]);
                // LF may occupy byte M; CR counts before ScanLines strips it.
                if out.len() >= MAX_IGNORE_LINE_BYTES {
                    return LineRead::Failed(ScanWarningKind::IgnoreLineTooLong);
                }
            }
            Err(_) => return LineRead::Failed(ScanWarningKind::IgnoreFileReadFailed),
        }
    }
}

/// The `.ndignore` file name, as an `OsStr` for path joins.
#[must_use]
pub fn ignore_file_name() -> &'static OsStr {
    OsStr::new(SCAN_IGNORE_FILE)
}
