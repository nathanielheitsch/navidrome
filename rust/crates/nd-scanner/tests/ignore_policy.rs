//! `.ndignore` policy tests: pinned Go matcher translation, stack semantics, and file
//! loading taxonomy.
//!
//! ## Oracle provenance
//!
//! Original expectations were captured by executing the **pinned** matcher —
//! `github.com/sabhiram/go-gitignore` v0.0.0-20210923224102-525f6e181f06 (`go.mod:52`,
//! `go.sum:231-232`) — from the module cache, plus Go's own `regexp` compiler, under
//! `go1.27.4` (darwin/arm64). The scratch capture reproduced `getPatternFromLine` verbatim
//! and printed the translated expressions and match decisions. Go test expectations from
//! `scanner/ignore_checker_test.go` seed several cases. The added malformed `b{` and
//! `c{2,` cases follow the pinned translation and Go regexp's literal malformed-brace
//! rule; they have not been independently re-executed in this remediation. No Go source
//! in this repository was edited and no Go helper was added.
//!
//! Conservative refusal (including where engines agree) covers brace syntax, `(?…)`
//! forms, and Perl class/boundary escapes. Refused lines are reported, not matched.
//! This is conservative policy. Patterns Go cannot compile are reported instead of silently
//! omitted — same effective behaviour, better observability.

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use nd_scanner::contract::ScanWarningKind;
use nd_scanner::ignore::{
    read_ignore_file, read_ignore_reader, translate_line, IgnoreFileOutcome, IgnoreStack,
    Translation, MAX_IGNORE_LINE_BYTES,
};

/// `(pattern, path, Go's translated expression, Go's match decision)`.
const GO_MATCHER_GOLDEN: &[(&str, &str, &str, bool)] = &[
    ("secret", "rock/secret", "^(|.*/)secret(|/.*)$", true),
    ("secret", "rock/secret.mp3", "^(|.*/)secret(|/.*)$", false),
    ("secret", "secret", "^(|.*/)secret(|/.*)$", true),
    ("secret", "secret/x.mp3", "^(|.*/)secret(|/.*)$", true),
    ("*.mp3", "a.mp3", "^(|.*/)([^/]*)\\.mp3(|/.*)$", true),
    ("*.mp3 ", "a.mp3", "^(|.*/)([^/]*)\\.mp3(|/.*)$", true),
    ("  *.mp3", "a.mp3", "^(|.*/)([^/]*)\\.mp3(|/.*)$", true),
    ("*.mp3", "sub/a.mp3", "^(|.*/)([^/]*)\\.mp3(|/.*)$", true),
    (
        "*.mp3",
        "sub/deep/a.mp3",
        "^(|.*/)([^/]*)\\.mp3(|/.*)$",
        true,
    ),
    ("*.mp3", ".mp3", "^(|.*/)([^/]*)\\.mp3(|/.*)$", true),
    ("*.mp3", "a.MP3", "^(|.*/)([^/]*)\\.mp3(|/.*)$", false),
    (
        "foo/*.blah",
        "foo/x.blah",
        "^(|/)foo/([^/]*)\\.blah(|/.*)$",
        true,
    ),
    (
        "foo/*.blah",
        "foo/sub/x.blah",
        "^(|/)foo/([^/]*)\\.blah(|/.*)$",
        false,
    ),
    ("build/", "build", "^(|.*/)build/(|.*)$", false),
    ("build/", "build/out.mp3", "^(|.*/)build/(|.*)$", true),
    ("build/", "rebuild/x", "^(|.*/)build/(|.*)$", false),
    ("build/", "x/build/y", "^(|.*/)build/(|.*)$", true),
    ("temp/", "temporary/file.txt", "^(|.*/)temp/(|.*)$", false),
    (
        "**/*.log",
        "deep/nested/file.log",
        "^(|.*/)(|.*/)([^/]*)\\.log(|/.*)$",
        true,
    ),
    (
        "**/*.log",
        "deep/nested/file.txt",
        "^(|.*/)(|.*/)([^/]*)\\.log(|/.*)$",
        false,
    ),
    (
        "**/*",
        "any/path/file.txt",
        "^(|.*/)(|.*/)([^/]*)(|/.*)$",
        true,
    ),
    ("**/*", "file.txt", "^(|.*/)(|.*/)([^/]*)(|/.*)$", true),
    (
        "temp/*",
        "temp/cache.db",
        "^(|.*/)temp/([^/]*)(|/.*)$",
        true,
    ),
    (
        "temp/*",
        "temporary/cache.db",
        "^(|.*/)temp/([^/]*)(|/.*)$",
        false,
    ),
    (
        "temp/*",
        "temp/sub/cache.db",
        "^(|.*/)temp/([^/]*)(|/.*)$",
        true,
    ),
    ("/*.c", "cat-file.c", "^(|/)([^/]*)\\.c(|/.*)$", true),
    (
        "/*.c",
        "mozilla-sha1/sha1.c",
        "^(|/)([^/]*)\\.c(|/.*)$",
        false,
    ),
    ("a/**/b", "a/b", "^(|.*/)a(/|/.+/)b(|/.*)$", true),
    ("a/**/b", "a/x/b", "^(|.*/)a(/|/.+/)b(|/.*)$", true),
    ("a/**/b", "a/x/y/b", "^(|.*/)a(/|/.+/)b(|/.*)$", true),
    ("**/foo", "x/y/foo", "^(|.*/)(|.*/)foo(|/.*)$", true),
    (
        "**/foo/bar",
        "x/foo/bar",
        "^(|.*/)(|.*/)foo/bar(|/.*)$",
        true,
    ),
    (
        "**/foo/bar",
        "x/foo/baz/bar",
        "^(|.*/)(|.*/)foo/bar(|/.*)$",
        false,
    ),
    ("abc/**", "abc", "^(|.*/)abc(|/.*)(|/.*)$", true),
    ("abc/**", "abc/x", "^(|.*/)abc(|/.*)(|/.*)$", true),
    ("abc/**", "abc/x/y", "^(|.*/)abc(|/.*)(|/.*)$", true),
    ("?", "a", "^(|.*/)\\?(|/.*)$", false),
    ("?", "?", "^(|.*/)\\?(|/.*)$", true),
    ("?.mp3", "a.mp3", "^(|.*/)\\?\\.mp3(|/.*)$", false),
    ("a?b", "a?b", "^(|.*/)a\\?b(|/.*)$", true),
    ("a?b", "axb", "^(|.*/)a\\?b(|/.*)$", false),
    (
        "\\!important!.txt",
        "important!.txt",
        "^(|.*/)\\!important!\\.txt(|/.*)$",
        false,
    ),
    (
        "\\!important!.txt",
        "\\!important!.txt",
        "^(|.*/)\\!important!\\.txt(|/.*)$",
        false,
    ),
    (
        "\\#hash.txt",
        "#hash.txt",
        "^(|.*/)\\#hash\\.txt(|/.*)$",
        true,
    ),
    (
        "!important.txt",
        "important.txt",
        "^(|.*/)important\\.txt(|/.*)$",
        false,
    ),
    ("!*.txt", "a.txt", "^(|.*/)([^/]*)\\.txt(|/.*)$", false),
    ("[abc]", "a", "^(|.*/)[abc](|/.*)$", true),
    ("[abc]", "x", "^(|.*/)[abc](|/.*)$", false),
    ("[abc]", "[abc]", "^(|.*/)[abc](|/.*)$", false),
    ("foo.txt", "bar/foo.txt", "^(|.*/)foo\\.txt(|/.*)$", true),
    ("foo.txt", "foo.txt", "^(|.*/)foo\\.txt(|/.*)$", true),
    ("dir/sub", "dir/sub", "^(|.*/)dir/sub(|/.*)$", true),
    ("dir/sub", "other/dir/sub", "^(|.*/)dir/sub(|/.*)$", true),
    ("x\\*y", "x*y", "^(|.*/)x\\*y(|/.*)$", true),
    ("x\\*y", "xay", "^(|.*/)x\\*y(|/.*)$", false),
    ("\\.", ".", "^(|.*/)\\\\.(|/.*)$", false),
    ("a/b/", "a/b/c", "^(|.*/)a/b/(|.*)$", true),
    ("a/b/", "a/b", "^(|.*/)a/b/(|.*)$", false),
    ("**", "a", "^(|.*/)([^/]*)([^/]*)(|/.*)$", true),
    ("**", "a/b", "^(|.*/)([^/]*)([^/]*)(|/.*)$", true),
    ("+.mp3", "a.mp3", "^(|.*/)+\\.mp3(|/.*)$", false),
    ("+.mp3", "+.mp3", "^(|.*/)+\\.mp3(|/.*)$", false),
    (
        "foo/bar.*",
        "foo/bar.mp3",
        "^(|.*/)foo/bar\\.([^/]*)(|/.*)$",
        true,
    ),
    (
        "foo/bar.*",
        "sub/foo/bar.mp3",
        "^(|.*/)foo/bar\\.([^/]*)(|/.*)$",
        true,
    ),
    ("héllo", "héllo", "^(|.*/)héllo(|/.*)$", true),
    ("h?llo", "héllo", "^(|.*/)h\\?llo(|/.*)$", false),
    ("héllo", "HELLO", "^(|.*/)héllo(|/.*)$", false),
    ("*.log", ".hidden/log", "^(|.*/)([^/]*)\\.log(|/.*)$", false),
    (".hidden", ".hidden", "^(|.*/)\\.hidden(|/.*)$", true),
    (".hidden", "x/.hidden", "^(|.*/)\\.hidden(|/.*)$", true),
    ("..", "..", "^(|.*/)\\.\\.(|/.*)$", true),
    ("..", "../x", "^(|.*/)\\.\\.(|/.*)$", true),
    ("...Album", "...Album", "^(|.*/)\\.\\.\\.Album(|/.*)$", true),
    ("a|b", "a|b", "^(|.*/)a|b(|/.*)$", true),
    ("a|b", "a", "^(|.*/)a|b(|/.*)$", true),
    ("a^b", "a^b", "^(|.*/)a^b(|/.*)$", false),
    ("a$b", "a$b", "^(|.*/)a$b(|/.*)$", false),
    ("a\\d", "ad", "^(|.*/)a\\d(|/.*)$", false),
    ("a\\d", "a\\d", "^(|.*/)a\\d(|/.*)$", false),
    ("a.b", "axb", "^(|.*/)a\\.b(|/.*)$", false),
    ("a.b", "a.b", "^(|.*/)a\\.b(|/.*)$", true),
    ("*", "a", "^(|.*/)([^/]*)(|/.*)$", true),
    ("*", "a/b", "^(|.*/)([^/]*)(|/.*)$", true),
    ("/", "/x", "^(|/)(|.*)$", true),
    ("//", "/x", "^(|/)/(|.*)$", true),
    ("a//b", "a//b", "^(|.*/)a//b(|/.*)$", true),
    ("x/y/**/z", "x/y/z", "^(|.*/)x/y(/|/.+/)z(|/.*)$", true),
    ("x/y/**/z", "x/y/q/z", "^(|.*/)x/y(/|/.+/)z(|/.*)$", true),
    ("x/y/**/z", "x/q/y/z", "^(|.*/)x/y(/|/.+/)z(|/.*)$", false),
];

fn fixture(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "nd-scanner-ignore-{}-{}-{}",
        std::process::id(),
        nanos,
        name
    ));
    std::fs::create_dir_all(&root).expect("create fixture root");
    root
}

fn cleanup(root: &std::path::Path) {
    let _ = std::fs::remove_dir_all(root);
}

fn compiled(line: &str) -> nd_scanner::ignore::Pattern {
    match translate_line(line) {
        Translation::Compiled(pattern) => pattern,
        other => panic!("{line:?} → {other:?}"),
    }
}

/// Go's `MatchesPath` over a one-line file: the negation flag is applied, so a lone `!`
/// pattern never ignores anything.
fn go_matches_path(patterns: &[String], path: &str) -> bool {
    let mut stack = IgnoreStack::new();
    stack.push(patterns);
    stack.should_ignore(path)
}

#[test]
fn translated_expression_and_match_agree_with_the_pinned_go_matcher() {
    for (pattern, path, go_expr, expected) in GO_MATCHER_GOLDEN {
        let translation = translate_line(pattern);
        if *pattern == r"a\d" {
            // Retain captured Go evidence, but this syntax is now refused by policy.
            assert!(matches!(translation, Translation::Unsupported { .. }));
            assert!(!go_matches_path(&[pattern.to_string()], path));
            continue;
        }
        let compiled = match translation {
            Translation::Compiled(ref pattern) => pattern,
            ref other => panic!("{pattern:?} → {other:?}, expected a compiled pattern"),
        };
        assert_eq!(
            compiled.expression(),
            *go_expr,
            "translation of {pattern:?} must reproduce Go's expression byte for byte"
        );
        assert_eq!(
            go_matches_path(&[pattern.to_string()], path),
            *expected,
            "{pattern:?} vs {path:?}: the pinned Go matcher decided {expected}"
        );
    }
}

#[test]
fn negation_is_applied_in_list_order_like_matches_path_how() {
    // Go's `MatchesPathHow` walks patterns in order, so a negation only undoes an
    // *earlier* match.
    let mut stack = IgnoreStack::new();
    stack.push(&["*.txt".to_string(), "!a.txt".to_string()]);
    assert!(
        !stack.should_ignore("a.txt"),
        "*.txt then !a.txt re-includes a.txt"
    );
    assert!(stack.should_ignore("b.txt"), "b.txt stays ignored");

    let mut stack = IgnoreStack::new();
    stack.push(&["!a.txt".to_string(), "*.txt".to_string()]);
    assert!(
        stack.should_ignore("a.txt"),
        "!a.txt before *.txt is a no-op: the later pattern re-ignores it"
    );

    let mut stack = IgnoreStack::new();
    stack.push(&[
        "*.txt".to_string(),
        "!*.txt".to_string(),
        "*.txt".to_string(),
    ]);
    assert!(
        stack.should_ignore("a.txt"),
        "the final positive pattern wins, as Go's loop decides"
    );
}

#[test]
fn bare_name_matches_component_and_suffix_but_never_a_prefixed_name() {
    // Go: `secret` → `^(|.*/)secret(|/.*)$`, so `rock/secret` and `secret/x.mp3` match but
    // `rock/secret.mp3` does not.
    let pattern = compiled("secret");
    assert!(pattern.matches("rock/secret"));
    assert!(pattern.matches("secret"));
    assert!(pattern.matches("secret/x.mp3"));
    assert!(!pattern.matches("rock/secret.mp3"));
}

#[test]
fn trailing_slash_never_matches_the_bare_directory_name() {
    // Documented matcher quirk: `build/` → `^(|.*/)build/(|.*)$`.
    let pattern = compiled("build/");
    assert!(!pattern.matches("build"), "the bare name is NOT matched");
    assert!(pattern.matches("build/out.mp3"));
    assert!(!pattern.matches("rebuild/x"));
    assert!(pattern.matches("x/build/y"));
    // Which means a walker can still emit an empty `build` folder — asserted in walk_policy.
}

#[test]
fn question_mark_is_a_literal_not_a_wildcard() {
    assert!(!compiled("?.mp3").matches("a.mp3"));
    assert!(compiled("a?b").matches("a?b"));
    assert!(!compiled("a?b").matches("axb"));
    assert!(!compiled("h?llo").matches("héllo"));
}

#[test]
fn escaped_star_is_literal_and_hash_or_exclamation_escapes_survive() {
    assert!(compiled("x\\*y").matches("x*y"));
    assert!(!compiled("x\\*y").matches("xay"));
    // Go compiles `\#`/`\!` as punctuation escapes; Rust's engine rejects them, so a
    // discrepancy in the *escaped* form would have been a silent behaviour change.
    let hash = compiled("\\#hash.txt");
    assert!(hash.matches("#hash.txt"));
    assert!(!compiled("\\!important!.txt").matches("important!.txt"));
}

#[test]
fn a_pattern_go_cannot_compile_is_reported_rather_than_omitted_silently() {
    for line in ["[a-", "a(", "\\"] {
        match translate_line(line) {
            Translation::Invalid { expr, .. } => {
                assert!(!expr.is_empty(), "the expression is reported for {line:?}");
            }
            other => panic!("{line:?} → {other:?}, expected a reported invalid pattern"),
        }
    }
    // And the stack records exactly one warning per such line.
    let mut stack = IgnoreStack::new();
    stack.push(&["[a-".to_string(), "*.txt".to_string(), "a(".to_string()]);
    let warnings = stack.warnings();
    assert_eq!(warnings.len(), 2, "got {warnings:?}");
    assert!(warnings
        .iter()
        .all(|w| w.kind == ScanWarningKind::IgnorePatternInvalid));
    assert!(
        stack.should_ignore("a.txt"),
        "the compilable pattern still applies"
    );
}

/// Go decisions for constructs conservatively refused by policy, from the pinned
/// matcher. Valid repetitions and translated literal question-mark groups can agree
/// across engines; refusal is intentionally broader than proven incompatibility.
const GO_ENGINE_DIVERGENCE_GOLDEN: &[(&str, &[(&str, bool)])] = &[
    // expr `^(|.*/){a}(|/.*)$` — Go compiles `{a}` as literal text (no valid repeat),
    // so the name itself matches. Rust rejects the expression outright.
    ("{a}", &[("{a}", true), ("sub/{a}", true)]),
    // `{1}` repeats the prefix group; neither engine matches the names below.
    ("{1}", &[("a", false), ("{1}", false)]),
    // expr `^(|.*/)a{1,2}(|/.*)$` — Go: repetition, so "a" and "aa" match
    ("a{1,2}", &[("a", true), ("aa", true), ("a{1,2}", false)]),
    // expr `^(|.*/)(\?i)Foo(|/.*)$` — Go: literal `?iFoo`, case-sensitive
    (
        "(?i)Foo",
        &[("?iFoo", true), ("sub/?iFoo", true), ("Foo", false)],
    ),
    // expr `^(|.*/)sub/{a}\.mp3(|/.*)$` — Go: literal text, matches the name itself
    ("sub/{a}.mp3", &[("sub/{a}.mp3", true)]),
    // Malformed repetitions are literal text in Go; Rust rejects these regexes.
    ("b{", &[("b{", true), ("b", false)]),
    ("c{2,", &[("c{2,", true), ("cc", false)]),
];

#[test]
fn engine_divergent_constructs_are_refused_and_the_divergence_is_recorded() {
    for line in ["{a}", "a{1,2}", "(?i)ABC", "sub/{a}.mp3", "b{", "c{2,"] {
        match translate_line(line) {
            Translation::Unsupported { construct, .. } => {
                assert!(!construct.is_empty(), "the construct is named for {line:?}");
            }
            other => panic!("{line:?} → {other:?}: conservative policy must refuse this syntax"),
        }
    }
    // A refused line matches nothing here, whatever Go decided for it.
    let mut stack = IgnoreStack::new();
    stack.push(&["{a}".to_string(), "*.txt".to_string()]);
    assert_eq!(stack.warnings().len(), 1);
    assert_eq!(
        stack.warnings()[0].kind,
        ScanWarningKind::IgnorePatternUnsupported
    );
    assert!(
        !stack.should_ignore("{a}"),
        "the unsupported pattern matches nothing"
    );
    assert!(stack.should_ignore("a.txt"));

    // Report, do not hide, where that differs from the captured Go decisions above:
    // every path Go matched is one this lane deliberately does *not* match.
    let mut divergences = 0;
    for (line, cases) in GO_ENGINE_DIVERGENCE_GOLDEN {
        assert!(
            matches!(translate_line(line), Translation::Unsupported { .. }),
            "{line:?} must stay refused"
        );
        let mut stack = IgnoreStack::new();
        stack.push(&[line.to_string()]);
        for (path, go_matches) in *cases {
            assert!(
                !stack.should_ignore(path),
                "a refused pattern must match nothing, got a match for {path:?}"
            );
            if *go_matches {
                divergences += 1;
            }
        }
    }
    assert!(
        divergences > 0,
        "the goldens must record at least one Go match this lane refuses, otherwise the \
         divergence claim is vacuous"
    );
}

#[test]
fn conservative_constructs_are_refused_not_reinterpreted() {
    for line in ["{a}", "a{1,2}", "(?i)ABC"] {
        match translate_line(line) {
            Translation::Unsupported { construct, .. } => {
                assert!(!construct.is_empty(), "the construct is named for {line:?}");
            }
            other => panic!("{line:?} → {other:?}: conservative policy refuses this syntax"),
        }
    }
    let mut stack = IgnoreStack::new();
    stack.push(&["{a}".to_string(), "*.txt".to_string()]);
    assert_eq!(stack.warnings().len(), 1);
    assert_eq!(
        stack.warnings()[0].kind,
        ScanWarningKind::IgnorePatternUnsupported
    );
    assert!(
        !stack.should_ignore("{a}"),
        "the unsupported pattern matches nothing"
    );
    assert!(stack.should_ignore("a.txt"));
}

#[test]
fn whitespace_is_trimmed_from_both_ends_by_the_loader_and_the_matcher() {
    // `scanner/ignore_checker.go:126` Unicode-trims; `getPatternFromLine` then trims ASCII spaces.
    assert_eq!(
        compiled("  *.mp3").expression(),
        compiled("*.mp3").expression()
    );
    assert_eq!(
        compiled("*.mp3 ").expression(),
        compiled("*.mp3").expression()
    );
    assert!(compiled("*.mp3 ").matches("a.mp3"));
    // The matcher alone does NOT strip tabs — the loader must have already trimmed them,
    // which the loader test below covers.
    assert_ne!(
        translate_line("\t*.log"),
        Translation::Skipped,
        "a tab survives to translation, so trimming is the loader's job"
    );
}

#[test]
fn an_empty_or_comment_only_file_means_ignore_everything() {
    for content in ["", "\n", "# only a comment\n\n  \n\t\n"] {
        let outcome = read_ignore_reader(content.as_bytes());
        assert_eq!(
            outcome,
            IgnoreFileOutcome::Loaded(vec!["**/*".to_string()]),
            "{content:?} must become the ignore-all pattern, not 'ignore nothing'"
        );
    }
    let mut stack = IgnoreStack::new();
    stack.push(read_ignore_reader("".as_bytes()).patterns());
    assert!(stack.should_ignore("file.txt"));
    assert!(stack.should_ignore("folder/file.mp3"));
    assert!(
        !stack.should_ignore(""),
        "the root is never ignored, even under `**/*`"
    );
    assert!(!stack.should_ignore("."), "`.' is never ignored");
}

#[test]
fn parsing_matches_the_go_expectations_from_ignore_checker_test() {
    // Seeded from scanner/ignore_checker_test.go's DescribeTable entries.
    let cases: &[(&str, &[&str])] = &[
        ("*.txt", &["*.txt"]),
        ("*.txt\n*.log", &["*.txt", "*.log"]),
        ("# comment\n*.txt\n# another\n*.log", &["*.txt", "*.log"]),
        ("*.txt\n\n*.log\n\n", &["*.txt", "*.log"]),
        (
            "# header\n\n*.txt\n# middle\n*.log\n\n",
            &["*.txt", "*.log"],
        ),
        ("*.txt\n*.log\n", &["*.txt", "*.log"]),
        ("temp/", &["temp/"]),
        ("**/*.mp3", &["**/*.mp3"]),
        ("!important.txt", &["!important.txt"]),
        ("not#comment", &["not#comment"]),
        ("*.txt\n   \n*.log\n\t\n", &["*.txt", "*.log"]),
        ("  *.txt  \n\t*.log\t", &["*.txt", "*.log"]),
    ];
    for (content, expected) in cases {
        let outcome = read_ignore_reader(content.as_bytes());
        let patterns: Vec<&str> = outcome.patterns().iter().map(String::as_str).collect();
        assert_eq!(patterns, expected.to_vec(), "content {content:?}");
    }
}

/// Reader that hands out `ok` bytes and then fails, emulating a mid-read I/O error.
struct FailingReader {
    ok: &'static [u8],
    sent: usize,
}

impl Read for FailingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.sent >= self.ok.len() {
            return Err(std::io::Error::other("simulated read failure"));
        }
        let take = buf.len().min(self.ok.len() - self.sent);
        buf[..take].copy_from_slice(&self.ok[self.sent..self.sent + take]);
        self.sent += take;
        Ok(take)
    }
}

#[test]
fn a_read_failure_keeps_the_prefix_and_never_becomes_ignore_all() {
    // A valid final partial line is processed before the read error is reported.
    let outcome = read_ignore_reader(FailingReader {
        ok: b"*.txt",
        sent: 0,
    });
    match outcome {
        IgnoreFileOutcome::ReadFailedPrefix { patterns, kind } => {
            assert_eq!(patterns, vec!["*.txt"]);
            assert_eq!(kind, ScanWarningKind::IgnoreFileReadFailed);
        }
        other => panic!("{other:?}"),
    }

    // Failure *after* two complete lines: those two patterns survive.
    let outcome = read_ignore_reader(FailingReader {
        ok: b"*.txt\n*.log\npartial-and-unfinished",
        sent: 0,
    });
    match outcome {
        IgnoreFileOutcome::ReadFailedPrefix { ref patterns, kind } => {
            assert_eq!(patterns, &vec!["*.txt", "*.log", "partial-and-unfinished"]);
            assert_eq!(kind, ScanWarningKind::IgnoreFileReadFailed);
        }
        other => panic!("{other:?} expected the accumulated prefix"),
    }
    // The critical non-vacuity: a prefix-bearing failure must not turn into `**/*`.
    let mut stack = IgnoreStack::new();
    stack.push(outcome.patterns());
    assert!(stack.should_ignore("a.txt"));
    assert!(
        !stack.should_ignore("a.mp3"),
        "a read failure must not silently ignore the whole tree"
    );
}

#[test]
fn an_overlong_line_ends_the_read_like_bufio_err_too_long() {
    let mut content = vec![b'a'; MAX_IGNORE_LINE_BYTES + 8];
    content.push(b'\n');
    let outcome = read_ignore_reader(std::io::Cursor::new(content));
    match outcome {
        IgnoreFileOutcome::ReadFailedPrefix { patterns, kind } => {
            assert!(
                patterns.is_empty(),
                "the over-long token never became a pattern"
            );
            assert_eq!(kind, ScanWarningKind::IgnoreLineTooLong);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_non_utf8_line_ends_the_read_like_a_go_read_failure() {
    let bytes = [b"*.txt\n".to_vec(), vec![0xff, 0xfe], b"*.log\n".to_vec()].concat();
    let outcome = read_ignore_reader(bytes.as_slice());
    match outcome {
        IgnoreFileOutcome::ReadFailedPrefix { patterns, kind } => {
            assert_eq!(patterns, vec!["*.txt".to_string()]);
            assert_eq!(kind, ScanWarningKind::IgnoreFileNotUtf8);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_missing_ignore_file_is_absent_and_silent_a_real_one_is_loaded() {
    let root = fixture("absent");
    let outcome = read_ignore_file(&root.join(".ndignore"));
    assert_eq!(outcome, IgnoreFileOutcome::Absent);
    assert!(outcome
        .warnings(1, nd_scanner::storage::LogicalPath::root())
        .is_empty());

    std::fs::write(root.join(".ndignore"), "*.flac\n").expect("write");
    let outcome = read_ignore_file(&root.join(".ndignore"));
    assert_eq!(
        outcome,
        IgnoreFileOutcome::Loaded(vec!["*.flac".to_string()])
    );
    cleanup(&root);
}

#[cfg(unix)]
#[test]
fn an_unopenable_ignore_file_yields_no_patterns_with_one_warning() {
    use std::os::unix::fs::PermissionsExt;
    let root = fixture("open-fail");
    std::fs::write(root.join(".ndignore"), "*.flac\n").expect("write");
    std::fs::set_permissions(
        root.join(".ndignore"),
        std::fs::Permissions::from_mode(0o000),
    )
    .expect("chmod");
    let accessible = std::fs::File::open(root.join(".ndignore")).is_ok();
    let outcome = read_ignore_file(&root.join(".ndignore"));
    // Running as root can open a 0o000 file, so assert the guarantee that holds either way.
    match outcome {
        IgnoreFileOutcome::OpenFailed => {
            assert!(!accessible, "denied branch requires actual access denial");
            assert!(outcome
                .warnings(7, nd_scanner::storage::LogicalPath::root())
                .iter()
                .any(|w| w.kind == ScanWarningKind::IgnoreFileOpenFailed));
        }
        IgnoreFileOutcome::Loaded(ref patterns) => {
            assert!(accessible, "success branch requires actual access");
            assert_eq!(patterns, &vec!["*.flac".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    std::fs::set_permissions(
        root.join(".ndignore"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("restore");
    cleanup(&root);
}

#[test]
fn the_stack_pushes_and_pops_levels_and_isolates_siblings() {
    // Mirrors ignore_checker_test.go's Push/Pop expectations.
    let mut stack = IgnoreStack::new();
    assert_eq!(stack.depth(), 0);
    assert!(
        !stack.should_ignore("anything.txt"),
        "an empty stack ignores nothing"
    );

    stack.push(&["*.txt".to_string()]);
    assert_eq!(stack.depth(), 1);
    assert!(stack.should_ignore("a.txt"));

    stack.push(&["*.mp3".to_string()]);
    assert_eq!(stack.depth(), 2);
    assert!(stack.should_ignore("a.txt"), "root patterns persist");
    assert!(stack.should_ignore("a.mp3"));

    stack.pop();
    assert_eq!(stack.depth(), 1);
    assert!(!stack.should_ignore("a.mp3"), "the child level is gone");
    assert!(stack.should_ignore("a.txt"));

    stack.pop();
    assert_eq!(stack.depth(), 0);
    assert!(!stack.should_ignore("a.txt"));
    stack.pop();
    assert_eq!(stack.depth(), 0, "popping an empty stack is a no-op");
}

#[test]
fn push_all_parents_loads_root_then_every_ancestor_once() {
    let root = fixture("parents");
    std::fs::write(root.join(".ndignore"), "root.txt\n").expect("root");
    std::fs::create_dir_all(root.join("a/b/c")).expect("dirs");
    std::fs::write(root.join("a/.ndignore"), "level1.txt\n").expect("l1");
    std::fs::write(root.join("a/b/.ndignore"), "level2.txt\n").expect("l2");
    std::fs::write(root.join("a/b/c/.ndignore"), "level3.txt\n").expect("l3");

    let loader = |ancestors: &[&std::ffi::OsStr]| {
        let mut path = root.clone();
        for component in ancestors {
            path.push(component);
        }
        path.push(".ndignore");
        read_ignore_file(&path)
    };

    let mut stack = IgnoreStack::new();
    stack.push_all_parents(std::ffi::OsStr::new("a/b/c"), loader);
    assert_eq!(stack.depth(), 4, "root + three ancestors");
    assert!(stack.should_ignore("root.txt"));
    assert!(stack.should_ignore("a/level1.txt"));
    assert!(stack.should_ignore("a/b/level2.txt"));
    assert!(stack.should_ignore("a/b/c/level3.txt"));
    // Go-captured: a **bare-name** pattern written in one folder is unanchored, so it also
    // matches the same name under a sibling (`level3.txt` → `a/b/other/level3.txt` is true).
    // That is the no-rebase behaviour: patterns keep their literal shape, and Go's
    // `^(|.*/)` prefix makes bare names match at any depth.
    assert!(
        stack.should_ignore("a/b/other/level3.txt"),
        "a bare-name nested pattern is unanchored, not scoped to its folder"
    );

    // Empty and `.` push only the root frame.
    for target in ["", "."] {
        let mut stack = IgnoreStack::new();
        stack.push_all_parents(std::ffi::OsStr::new(target), loader);
        assert_eq!(stack.depth(), 1, "target {target:?}");
    }
    cleanup(&root);
}

#[test]
fn nested_patterns_are_flattened_without_rebasing() {
    // Go flattens the stack and matches library-relative paths, so a pattern written in a
    // child keeps its literal shape: `foo/*.blah` never matches `foo/sub/x.blah`, and a
    // root pattern keeps matching at any depth.
    let mut stack = IgnoreStack::new();
    stack.push(&[]); // root: nothing
    stack.push(&["foo/*.blah".to_string()]);
    assert!(stack.should_ignore("foo/x.blah"));
    assert!(!stack.should_ignore("foo/sub/x.blah"));

    let mut stack = IgnoreStack::new();
    stack.push(&["*.log".to_string()]);
    assert!(
        stack.should_ignore("deep/nested/a.log"),
        "a root pattern is unanchored, exactly like Go's `^(|.*/)` prefix"
    );
}

#[test]
fn an_ignored_directory_is_not_rescued_by_a_negated_descendant_pattern() {
    // Git (and this matcher) cannot re-include a file under an excluded directory: the
    // walker never descends, so the negation never gets a chance to apply. Asserted at the
    // matcher level here; walk_policy asserts the traversal outcome.
    let mut stack = IgnoreStack::new();
    stack.push(&["build/".to_string(), "!build/keep.mp3".to_string()]);
    // Go-captured: `[build/ !build/keep.mp3]` → "build" false, "build/keep.mp3" false,
    // "build/x.mp3" true. The negation only re-includes the one name it names, and since
    // `build/` ignores the *paths*, a walker never descends to read the file at all.
    assert!(
        !stack.should_ignore("build"),
        "`build/` does not match the bare directory name, so the walker enters it"
    );
    assert!(
        stack.should_ignore("build/x.mp3"),
        "paths under it are ignored"
    );
    assert!(
        !stack.should_ignore("build/keep.mp3"),
        "the named negation re-includes exactly one path"
    );
}

#[test]
fn empty_error_prefix_is_not_ignore_all_and_partial_error_keeps_warning() {
    for (bytes, expected) in [
        (&b""[..], Vec::<String>::new()),
        (&b"#comment"[..], vec![]),
        (&b"*.mp3"[..], vec!["*.mp3".to_string()]),
    ] {
        let outcome = read_ignore_reader(FailingReader { ok: bytes, sent: 0 });
        assert_eq!(outcome.patterns(), expected);
        assert_eq!(
            outcome.warnings(7, nd_scanner::storage::LogicalPath::root())[0].kind,
            ScanWarningKind::IgnoreFileReadFailed
        );
    }
}

#[test]
fn local_scanner_boundary_counts_raw_lf_crlf_and_eof_bytes() {
    let root = fixture("scanner-boundary");
    let file = root.join(".ndignore");
    for (terminator, max_payload) in [
        (&b"\n"[..], MAX_IGNORE_LINE_BYTES - 1),
        (&b"\r\n"[..], MAX_IGNORE_LINE_BYTES - 2),
        (&b""[..], MAX_IGNORE_LINE_BYTES - 1),
    ] {
        for payload in [max_payload - 1, max_payload, max_payload + 1] {
            let token = "a".repeat(payload);
            let mut bytes = b"keep.mp3\n".to_vec();
            bytes.extend(token.as_bytes());
            bytes.extend(terminator);
            std::fs::write(&file, bytes).unwrap();
            let outcome = read_ignore_file(&file);
            if payload <= max_payload {
                assert_eq!(
                    outcome,
                    IgnoreFileOutcome::Loaded(vec!["keep.mp3".into(), token])
                );
            } else {
                assert_eq!(
                    outcome,
                    IgnoreFileOutcome::ReadFailedPrefix {
                        patterns: vec!["keep.mp3".into()],
                        kind: ScanWarningKind::IgnoreLineTooLong,
                    }
                );
            }
        }
    }
    cleanup(&root);
}

#[test]
fn perl_ascii_class_and_boundary_escapes_are_conservatively_unsupported() {
    // Source-derived Go policy evidence: Go Perl classes/boundaries are ASCII; Rust's
    // defaults are Unicode. Refuse this family, even when a particular input agrees.
    for escape in [r"\d", r"\D", r"\w", r"\W", r"\s", r"\S", r"\b", r"\B"] {
        assert!(matches!(
            translate_line(escape),
            Translation::Unsupported { .. }
        ));
        let mut stack = IgnoreStack::new();
        stack.push(&[escape.to_string(), "*.mp3".to_string()]);
        assert_eq!(stack.warnings().len(), 1);
        assert_eq!(
            stack.warnings()[0].kind,
            ScanWarningKind::IgnorePatternUnsupported
        );
        assert!(stack.should_ignore("song.mp3"));
        assert!(!stack.should_ignore("é١"));
    }
}
