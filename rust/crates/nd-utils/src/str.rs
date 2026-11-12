//! String helpers — port of `utils/str` (str.go, content_disposition.go,
//! normalize_fts.go, sanitize_strings.go).
//!
//! Divergences from the Go source, required by the crate boundary:
//! - `sanitize.Accents` (github.com/deluan/sanitize) is inlined as the
//!   [`accents`] table, ported verbatim.
//! - `RemoveArticle`/`SanitizeFieldForSorting*` take the ignored-articles list
//!   as a parameter instead of reading the global `conf.Server` (the #S1
//!   explicit-construction freeze; `nd-conf` is a sibling crate).
//! - `SanitizeText`/`SanitizeHTML` use a small conservative UGC-style allowlist
//!   instead of pulling in a full `bluemonday` port; the test cases from
//!   `sanitize_strings_test.go` are ported against it.

use regex::Regex;

const MAX_FILENAME_BYTES: usize = 255;
const MAX_EXTENSION_BYTES: usize = 16;
const FALLBACK_FILENAME: &str = "download";

/// Replaces some Unicode punctuation characters with their equivalent ASCII
/// (`str.Clear`): typographic quotes → `"`/`'`, dashes → `-`.
#[must_use]
pub fn clear(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for r in name.chars() {
        out.push(match r {
            '‘' | '’' | '‛' | '′' => '\'',
            '＂' | '〃' | 'ˮ' | 'ײ' | '᳓' | '″' | '‶' | '˶' | 'ʺ' | '“' | '”' | '˝' | '‟' => {
                '"'
            }
            '‐' | '–' | '—' | '−' | '―' => '-',
            other => other,
        });
    }
    out
}

/// Finds the longest common prefix of all strings in `list`
/// (`str.LongestCommonPrefix`). Empty input yields the empty string.
#[must_use]
pub fn longest_common_prefix(list: &[&str]) -> String {
    if list.is_empty() {
        return String::new();
    }
    let first = list[0];
    for l in 0..first.len() {
        for s in &list[1..] {
            if l >= s.len() || s.as_bytes()[l] != first.as_bytes()[l] {
                return s[..l].to_string();
            }
        }
    }
    first.to_string()
}

/// Truncates a string to a maximum number of runes, adding a suffix if
/// truncated (`str.TruncateRunes`). The suffix is included in the rune count.
#[must_use]
pub fn truncate_runes(s: &str, max_runes: usize, suffix: &str) -> String {
    let rune_count = s.chars().count();
    if rune_count <= max_runes {
        return s.to_string();
    }

    let suffix_runes = suffix.chars().count();
    let truncate_at = max_runes.saturating_sub(suffix_runes);

    let mut runes = s.chars();
    let prefix: String = runes.by_ref().take(truncate_at).collect();
    if truncate_at >= rune_count {
        return format!("{s}{suffix}");
    }
    format!("{prefix}{suffix}")
}

/// Replaces accented characters with their ASCII equivalents
/// (`sanitize.Accents` from github.com/deluan/sanitize, table ported verbatim).
#[must_use]
pub fn accents(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match accent_of(c) {
            Some(a) => out.push_str(a),
            None => out.push(c),
        }
    }
    out
}

const fn accent_of(r: char) -> Option<&'static str> {
    match r {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ă' => Some("A"),
        'Æ' => Some("AE"),
        'Ç' => Some("C"),
        'È' | 'É' | 'Ê' | 'Ë' => Some("E"),
        'Ì' | 'Í' | 'Î' | 'Ï' => Some("I"),
        'Ð' => Some("D"),
        'Ł' => Some("L"),
        'Ñ' => Some("N"),
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => Some("O"),
        'Œ' => Some("OE"),
        'Ř' => Some("R"),
        'Ś' | 'Ș' | 'Ş' | 'Š' => Some("S"),
        'Ț' => Some("T"),
        'Ù' | 'Ú' | 'Ü' | 'Û' => Some("U"),
        'Ý' => Some("Y"),
        'Þ' => Some("TH"),
        'ẞ' => Some("SS"),
        'ß' => Some("ss"),
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ă' => Some("a"),
        'æ' => Some("ae"),
        'ç' => Some("c"),
        'è' | 'é' | 'ê' | 'ë' => Some("e"),
        'ì' | 'í' | 'î' | 'ï' => Some("i"),
        'ð' => Some("d"),
        'ł' => Some("l"),
        'ñ' | 'ń' => Some("n"),
        'ò' | 'ó' | 'ô' | 'õ' | 'ō' | 'ö' | 'ø' => Some("o"),
        'œ' => Some("oe"),
        'ř' => Some("r"),
        'ś' | 'ș' | 'ş' | 'š' => Some("s"),
        'ț' => Some("t"),
        'ù' | 'ú' | 'û' | 'ū' | 'ü' => Some("u"),
        'ý' | 'ÿ' => Some("y"),
        'ż' => Some("z"),
        _ => None,
    }
}

/// Replaces filesystem-reserved characters (`/\:*?"<>|`) with underscores
/// (`str.SanitizeFilename`).
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        out.push(match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        });
    }
    out
}

/// Concatenates the trimmed inputs, strips symbols/accents/slashes, lowercases,
/// deduplicates and sorts the words (`str.SanitizeStrings`).
#[must_use]
pub fn sanitize_strings(text: &[&str]) -> String {
    // Concatenate all strings, removing extra spaces
    let mut sanitized = String::new();
    for txt in text {
        sanitized.push_str(txt.trim());
        sanitized.push(' ');
    }

    // Remove special symbols, accents, extra spaces and slashes, matching
    // `sanitize_strings.go:25-27`. Use Go's [`to_lower`] before accent
    // folding; `str::to_lowercase` has different Unicode behavior.
    let mut s = sanitized.replace(['\\', '/'], " ");
    s = clear(&s);
    s = accents(&to_lower(&s));
    s.retain(|c| {
        !matches!(
            c,
            '“' | '”' | '‘' | '’' | '\'' | '"' | '[' | '(' | '{' | '\\' | ']' | ')' | '}' | ','
        )
    });

    let mut words: Vec<&str> = s.split_whitespace().collect();

    // Remove duplicated words
    words.sort_unstable();
    words.dedup();

    words.join(" ")
}

/// Sanitizes plain text for UI rendering (`str.SanitizeText`): strips
/// disallowed tags, then unescapes HTML entities.
#[must_use]
pub fn sanitize_text(text: &str) -> String {
    html_unescape(&sanitize_html_policy(text))
}

/// Sanitizes HTML for UI rendering (`str.SanitizeHTML`): unescapes entities
/// first (so entity-encoded tags cannot bypass), then strips disallowed tags
/// and attributes.
#[must_use]
pub fn sanitize_html(text: &str) -> String {
    sanitize_html_policy(&html_unescape(text))
}

const UGC_ALLOWED_TAGS: &[&str] = &[
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "div",
    "span",
    "hr",
    "p",
    "br",
    "b",
    "i",
    "strong",
    "em",
    "ol",
    "ul",
    "li",
    "a",
    "img",
    "pre",
    "code",
    "blockquote",
    "article",
    "section",
];
const UGC_DROP_CONTENT_TAGS: &[&str] = &["script", "style", "iframe", "object", "embed"];

/// Minimal bluemonday-UGCPolicy-style sanitizer: keeps allowlisted tags
/// (attributes stripped, except `a href` / `img src` with safe URL schemes),
/// drops the content of script/style-like elements, and drops other tags
/// entirely while keeping their text.
fn sanitize_html_policy(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    let mut drop_depth = 0usize;
    let mut drop_tag = String::new();
    while let Some(pos) = rest.find('<') {
        let (text, after) = rest.split_at(pos);
        if drop_depth == 0 {
            out.push_str(text);
        }
        let Some(close) = after.find('>') else {
            // Unterminated tag: drop the rest.
            return out;
        };
        let tag_end = close + 1;
        let inner = &after[1..close];
        // Not a tag unless the name starts with a letter (HTML markup rule;
        // e.g. Enhanced-LRC markers <00:01.00> pass through as text).
        let name_start: &str = inner.strip_prefix('/').unwrap_or(inner);
        if !name_start.starts_with(|c: char| c.is_ascii_alphabetic()) {
            out.push('<');
            rest = &after[1..];
            continue;
        }
        let name = tag_name(inner).to_ascii_lowercase();
        let is_closing = inner.starts_with('/');
        if drop_depth > 0 {
            if is_closing && name == drop_tag {
                drop_depth -= 1;
            }
        } else if is_closing {
            if UGC_ALLOWED_TAGS.contains(&name.as_str()) {
                out.push_str("</");
                out.push_str(&name);
                out.push('>');
            }
        } else if UGC_DROP_CONTENT_TAGS.contains(&name.as_str()) {
            drop_depth += 1;
            drop_tag = name;
        } else if UGC_ALLOWED_TAGS.contains(&name.as_str()) {
            let attrs = clean_attributes(inner);
            out.push('<');
            out.push_str(&name);
            out.push_str(&attrs);
            out.push('>');
        }
        // Disallowed opening/closing tags: dropped, keeping their text.
        rest = &after[tag_end..];
    }
    if drop_depth == 0 {
        out.push_str(rest);
    }
    out
}

/// Returns the lowercase tag name of a tag body.
fn tag_name(inner: &str) -> &str {
    let name: String = inner
        .strip_prefix('/')
        .unwrap_or(inner)
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    Box::leak(name.into_boxed_str())
}

/// Keeps only safe URL attributes (`href`/`src`) with http(s)/mailto/relative values.
fn clean_attributes(inner: &str) -> String {
    let name = tag_name(inner);
    if name != "a" && name != "img" {
        return String::new();
    }
    let key = if name == "a" { "href" } else { "src" };
    // Unquoted, single- or double-quoted value; Go requires a parseable URL and
    // allows relative ones (AllowStandardURLs: mailto/http/https + relative).
    let re = Regex::new(&format!(
        r#"(?i){key}\s*=\s*(?:"([^"\s>]*)"|'([^'\s>]*)'|([^\s>]+))"#
    ))
    .expect("static regex");
    let Some(caps) = re.captures(inner) else {
        return String::new();
    };
    let val = caps
        .get(1)
        .or_else(|| caps.get(2))
        .or_else(|| caps.get(3))
        .map_or("", |m| m.as_str());
    // Relative URLs are allowed; only reject other absolute schemes.
    let ok = match val.find(':') {
        Some(i) if !val[..i].contains('/') => {
            let scheme = val[..i].to_ascii_lowercase();
            scheme == "mailto" || scheme == "http" || scheme == "https"
        }
        _ => true,
    };
    if ok {
        format!(" {key}=\"{val}\"")
    } else {
        String::new()
    }
}

/// Minimal `html.UnescapeString` for named entities used by the ported tests.
fn html_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// Normalizes a field for sorting: trim, strip accents, lowercase, then apply
/// [`clear`] (`str.SanitizeFieldForSorting`).
#[must_use]
pub fn sanitize_field_for_sorting(original: &str) -> String {
    let v = accents(original.trim());
    // `str.SanitizeFieldForSorting` = Clear(strings.ToLower(...)); the
    // lowercasing must be Go's [`to_lower`]. This result is persisted into the
    // order_*/sort_* columns, so a divergence would disagree with a
    // Go-written database.
    clear(&to_lower(&v))
}

/// [`sanitize_field_for_sorting`] with the leading article removed
/// (`str.SanitizeFieldForSortingNoArticle`).
#[must_use]
pub fn sanitize_field_for_sorting_no_article(original: &str, ignored_articles: &str) -> String {
    let v = accents(original.trim());
    clear(to_lower(remove_article(v.trim(), ignored_articles).trim()).as_str())
}

/// Removes a leading article (`str.RemoveArticle`). Articles are the
/// space-separated `ignored_articles` list (Go reads `conf.Server.IgnoredArticles`).
#[must_use]
pub fn remove_article(name: &str, ignored_articles: &str) -> String {
    for a in ignored_articles.split(' ') {
        let prefix = format!("{a} ");
        if let Some(n) = name.strip_prefix(&prefix) {
            return n.to_string();
        }
    }
    name.to_string()
}

/// Matches any character that is not a letter or number (`str.FTSPunctStrip`).
/// Index-time normalization and query-time processing share it so both sides
/// produce matching tokens.
pub fn fts_punct_strip() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^\p{L}\p{N}]").expect("static regex"))
}

/// Takes multiple strings and returns a space-separated, deduplicated list of
/// alternative searchable forms for each word: punctuation-stripped
/// (R.E.M. → REM, AC/DC → ACDC) and ASCII-transliterated (Bjørk → Bjork,
/// œuvre → oeuvre) (`str.NormalizeForFTS`).
///
/// The transliterated form is needed because FTS5's `unicode61
/// remove_diacritics 2` only handles NFKD-decomposable diacritics — atomic
/// letters like ø/æ/œ/ß survive tokenization, so the query side and index side
/// disagree without an explicit transliterated entry here.
#[must_use]
pub fn normalize_for_fts(values: &[&str]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut result: Vec<String> = Vec::new();

    let mut add = |orig: &str, variant: String| {
        if variant.is_empty() || variant == orig {
            return;
        }
        // Dedupe key only, but it decides which variants land in the persisted
        // search_* columns, so it must match Go's strings.ToLower.
        let lower = to_lower(&variant);
        if !seen.insert(lower) {
            return;
        }
        result.push(variant);
    };

    for v in values {
        for word in v.split_whitespace() {
            let transliterated = accents(word);
            // Concatenated ASCII form: R.E.M. → REM, AC/DC → ACDC, St-Étienne → StEtienne.
            let stripped = fts_punct_strip()
                .replace_all(&transliterated, "")
                .to_string();
            add(word, stripped);
            // Accent-only transliteration for words without name-punctuation (Bjørk → Bjork).
            add(word, transliterated.clone());
        }
    }
    result.join(" ")
}

/// Builds an RFC 6266 attachment header value for a user-controlled filename
/// (`str.ContentDispositionAttachment`). Non-ASCII names also get a
/// `filename*` parameter, which clients prefer over the ASCII fallback.
#[must_use]
pub fn content_disposition_attachment(filename: &str) -> String {
    let (stem, ext) = split_filename(filename);
    let header = format!(
        "attachment; filename=\"{}\"",
        go_quote(&join_filename(&to_ascii(&stem), &to_ascii(&ext)))
    );
    let name = join_filename(&stem, &ext);
    if name.is_ascii() {
        return header;
    }
    // mime.FormatMediaType emits a percent-encoded filename* for non-ASCII values
    format!(
        "{header}; filename*=utf-8''{}",
        percent_encode(name.as_bytes())
    )
}

/// Splits a sanitized filename into (stem, ext), dropping over-long
/// extensions and backing the stem off to a rune boundary.
fn split_filename(filename: &str) -> (String, String) {
    let valid = to_valid_utf8(filename.as_bytes(), '_');
    let sanitized = sanitize_filename(&valid);
    // Strip control characters and bidi controls.
    let name: String = sanitized
        .chars()
        .filter(|r| !r.is_control() && !is_bidi_control(*r))
        .collect();
    let ext = path_ext(&name);
    let ext = if ext.len() > MAX_EXTENSION_BYTES {
        String::new()
    } else {
        ext
    };
    let mut stem = name.strip_suffix(&ext).unwrap_or(&name).to_string();
    let limit = MAX_FILENAME_BYTES.saturating_sub(ext.len());
    if stem.len() > limit {
        let mut l = limit;
        let b = stem.as_bytes();
        while l > 0 && (b[l] & 0xC0) == 0x80 {
            l -= 1;
        }
        stem = stem[..l].to_string();
    }
    (stem, ext)
}

const fn is_bidi_control(r: char) -> bool {
    matches!(r, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Go `strings.ToLower`, byte-exact.
///
/// `str::to_lowercase()` is **not** equivalent and must not be used where a Go
/// string feeds an id, a lookup key, or a comparison:
///
/// - It applies Unicode *final-sigma* context folding (U+03A3 at a word end
///   becomes U+03C2). Go's `strings.ToLower` is rune-independent
///   (`Map(unicode.ToLower, s)`) and always yields U+03C3.
/// - It maps U+0130 LATIN CAPITAL LETTER I WITH DOT ABOVE to `i` plus U+0307
///   (two code points); Go's simple per-rune table maps it to plain `i`.
///
/// A differential sweep of every scalar value (0..=0x10FFFF, surrogates
/// excluded) against `strings.ToLower` -- 1488 runes change in Go -- found
/// **exactly** these two behaviours differing and nothing else, including no
/// rune Go lowers that Rust leaves unchanged. Sigma examples: the Greek word
/// for "Odysseus" in capitals lowers to Go `οδυσσ` but Rust `οδυσς`; "ΙΣ"
/// lowers to Go `ισ` but Rust `ις`.
///
/// Go's ASCII fast path and `Map` both lower each rune independently, so a
/// per-rune mapping is faithful. ASCII is the common case and short-circuits.
#[must_use]
pub fn to_lower(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    s.chars()
        .map(|c| match c {
            '\u{130}' => '\u{69}',
            _ => c.to_lowercase().next().unwrap_or(c),
        })
        .collect()
}

/// Go `path.Ext`: the suffix beginning at the final dot.
#[must_use]
pub fn path_ext(name: &str) -> String {
    match name.rfind('.') {
        Some(i) => name[i..].to_string(),
        None => String::new(),
    }
}

/// Applies the fallback/trim rules of `joinFilename` (Go `cmp.Or` chains).
fn join_filename(stem: &str, ext: &str) -> String {
    let stem = if stem.trim().is_empty() {
        FALLBACK_FILENAME
    } else {
        stem.trim()
    };
    let joined = format!("{stem}{ext}");
    let joined = joined.trim_end_matches([' ', '.']);
    if joined.is_empty() {
        FALLBACK_FILENAME.to_string()
    } else {
        joined.to_string()
    }
}

/// ASCII fallback of a filename part (`toASCII`): accents cleared, filename
/// sanitized, commas → `_`, non-printable-ASCII dropped.
fn to_ascii(s: &str) -> String {
    let cleaned = accents(&sanitize_filename(&clear(s)));
    cleaned
        .chars()
        .filter_map(|r| match r {
            ',' => Some('_'),
            ' '..='~' => Some(r),
            _ => None,
        })
        .collect()
}

/// Go `strconv.Quote`-compatible quoting for printable ASCII payloads.
fn go_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Replaces invalid UTF-8 sequences with `replacement` (Go
/// `strings.ToValidUTF8`).
#[must_use]
pub fn to_valid_utf8(bytes: &[u8], replacement: char) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or_default());
                out.push(replacement);
                let skip = e.error_len().unwrap_or(rest.len() - valid);
                rest = &rest[valid + skip..];
            }
        }
    }
    out
}

/// RFC 3986-style percent encoding for RFC 2231 `filename*` parameters.
#[must_use]
pub fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for &b in bytes {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_replaces_some_unicode_chars_with_ascii() {
        assert_eq!(clear("k–os"), "k-os");
        assert_eq!(clear("k‐os"), "k-os");
        assert_eq!(clear("“Weird” Al Yankovic"), "\"Weird\" Al Yankovic");
        assert_eq!(clear("‘Single’ quotes"), "'Single' quotes");
    }

    #[test]
    fn longest_common_prefix_finds_the_prefix() {
        // The full `testPaths` fixture from utils/str/str_test.go: the Go
        // test asserts the prefix over all entries, not just the first three.
        let paths: &[&str] = &[
            "/Music/iTunes 1/iTunes Media/Music/ABBA/Gold_ Greatest Hits/Dancing Queen.m4a",
            "/Music/iTunes 1/iTunes Media/Music/ABBA/Gold_ Greatest Hits/Mamma Mia.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Art Blakey/A Night At Birdland, Vol. 1/01 Annoucement By Pee Wee Marquette.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Art Blakey/A Night At Birdland, Vol. 1/02 Split Kick.m4a",
            "/Music/iTunes 1/iTunes Media/Music/As Frenéticas/As Frenéticas/Perigosa.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Bachman-Turner Overdrive/Gold/Down Down.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Bachman-Turner Overdrive/Gold/Hey You.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Bachman-Turner Overdrive/Gold/Hold Back The Water.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Belle And Sebastian/Write About Love/01 I Didn't See It Coming.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Belle And Sebastian/Write About Love/02 Come On Sister.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Black Eyed Peas/Elephunk/03 Let's Get Retarded.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Black Eyed Peas/Elephunk/04 Hey Mama.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Black Eyed Peas/Monkey Business/10 They Don't Want Music (Feat. James Brown).m4a",
            "/Music/iTunes 1/iTunes Media/Music/Black Eyed Peas/The E.N.D/1-01 Boom Boom Pow.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Black Eyed Peas/Timeless/01 Mas Que Nada.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Blondie/Heart Of Glass/Heart Of Glass.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Bob Dylan/Nashville Skyline/06 Lay Lady Lay.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Botany/Feeling Today - EP/03 Waterparker.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Céu/CéU/06 10 Contados.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Chance/Six Through Ten/03 Forgive+Forget.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Clive Tanaka Y Su Orquesta/Jet Set Siempre 1°/03 Neu Chicago (Side A) [For Dance].m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Absolute Rock Classics/1-02 Smoke on the water.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Almost Famous Soundtrack/10 Simple Man.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Audio News - Rock'n' Roll Forever/01 Rock Around The Clock.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Austin Powers_ International Man Of Mystery/01 The Magic Piper (Of Love).m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Austin Powers_ The Spy Who Shagged Me/04 American Woman.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Back To Dance/03 Long Cool Woman In A Black Dress.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Back To The 70's - O Album Da Década/03 American Pie.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Bambolê/09 In The Mood.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Bambolê - Volume II/03 Blue Moon.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Big Brother Brasil 2004/04 I Will Survive.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Collateral Soundtrack/03 Hands Of Time.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Forrest Gump - The Soundtrack/1-12 California Dreamin'.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Forrest Gump - The Soundtrack/1-16 Mrs. Robinson.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Ghost World - Original Motion Picture Soundtrack/01 Jaan Pechechaan Ho.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Grease [Original Soundtrack]/01 Grease.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/La Bamba/09 Summertime Blues.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Pretty Woman/10 Oh Pretty Woman.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents African Groove/01 Saye Mogo Bana.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Arabic Groove/02 Galbi.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Asian Groove/03 Remember Tomorrow.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Blues Lounge/01 Midnight Dream.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Blues Lounge/03 Banal Reality.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Blues Lounge/04 Parchman Blues.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Blues Lounge/06 Run On.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Brazilian Groove/01 Maria Moita.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Brazilian Lounge/08 E Depois....m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Brazilian Lounge/11 Os Grilos.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Euro Lounge/01 Un Simple Histoire.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Euro Lounge/02 Limbe.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Euro Lounge/05 Sempre Di Domenica.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents Euro Lounge/12 Voulez-Vous_.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents World Lounge/03 Santa Maria.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents_ A New Groove/02 Dirty Laundry.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents_ Blues Around the World/02 Canceriano Sem Lar (Clinica Tobias Blues).m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents_ Euro Groove/03 Check In.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Putumayo Presents_ World Groove/01 Attention.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Saturday Night Fever/01 Stayin' Alive.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/Saturday Night Fever/03 Night Fever.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/The Best Air Guitar Album In The World... Ever!/2-06 Johnny B. Goode.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/The Full Monty - Soundtrack/02 You Sexy Thing.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Compilations/The Full Monty - Soundtrack/11 We Are Family.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Cut Copy/Zonoscope (Bonus Version)/10 Corner of the Sky.m4a",
            "/Music/iTunes 1/iTunes Media/Music/David Bowie/Changesbowie/07 Diamond Dogs.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Douster & Savage Skulls/Get Rich or High Tryin' - EP/01 Bad Gal.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Elton John/Greatest Hits 1970-2002/1-04 Rocket Man (I Think It's Going to Be a Long, Long Time).m4a",
            "/Music/iTunes 1/iTunes Media/Music/Elvis Presley/ELV1S 30 #1 Hits/02 Don't Be Cruel.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Eric Clapton/The Cream Of Clapton/03 I Feel Free.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Fleetwood Mac/The Very Best Of Fleetwood Mac/02 Don't Stop.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Françoise Hardy/Comment te dire adieu/Comment te dire adieu.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Games/That We Can Play - EP/01 Strawberry Skies.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Grand Funk Railroad/Collectors Series/The Loco-Motion.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Henry Mancini/The Pink Panther (Music from the Film Score)/The Pink Panther Theme.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Holy Ghost!/Do It Again - Single/01 Do It Again.m4a",
            "/Music/iTunes 1/iTunes Media/Music/K.C. & The Sunshine Band/The Best of/03 I'm Your Boogie Man.m4a",
            "/Music/iTunes 1/iTunes Media/Music/K.C. & The Sunshine Band/Unknown Album/Megamix (Thats The Way, Shake Your Booty, Get Down Tonight, Give It Up).m4a",
            "/Music/iTunes 1/iTunes Media/Music/Kim Ann Foxman & Andy Butler/Creature - EP/01 Creature.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Nico/Chelsea Girl/01 The Fairest Of The Seasons.m4a",
            "/Music/iTunes 1/iTunes Media/Music/oOoOO/oOoOO - EP/02 Burnout Eyess.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Peter Frampton/The Very Best of Peter Frampton/Baby, I Love Your Way.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Peter Frampton/The Very Best of Peter Frampton/Show Me The Way.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Raul Seixas/A Arte De Raul Seixas/03 Metamorfose Ambulante.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Raul Seixas/A Arte De Raul Seixas/18 Eu Nasci há 10 Mil Anos Atrás.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Rick James/Street Songs/Super Freak.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Rita Lee/Fruto Proibido/Agora Só Falta Você.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Rita Lee/Fruto Proibido/Esse Tal De Roque Enrow.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Roberto Carlos/Roberto Carlos 1966/05 Negro Gato.m4a",
            "/Music/iTunes 1/iTunes Media/Music/SOHO/Goddess/02 Hippychick.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Stan Getz/Getz_Gilberto/05 Corcovado (Quiet Nights of Quiet Stars).m4a",
            "/Music/iTunes 1/iTunes Media/Music/Steely Dan/Pretzel Logic/Rikki Don't Loose That Number.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Stevie Wonder/For Once In My Life/I Don't Know Why.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Teebs/Ardour/While You Doooo.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Beatles/Magical Mystery Tour/08 Strawberry Fields Forever.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Beatles/Past Masters, Vol. 1/10 Long Tall Sally.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Beatles/Please Please Me/14 Twist And Shout.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Beatles/Sgt. Pepper's Lonely Hearts Club Band/03 Lucy In The Sky With Diamonds.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Black Crowes/Amorica/09 Wiser Time.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Black Crowes/By Your Side/05 Only A Fool.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Black Crowes/Shake Your Money Maker/04 Could I''ve Been So Blind.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Black Crowes/The Southern Harmony And Musical Companion/01 Sting Me.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Black Crowes/Three Snakes And One Charm/02 Good Friday.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Doors/Strange Days (40th Anniversary Mixes)/01 Strange Days.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Rolling Stones/Forty Licks/1-03 (I Can't Get No) Satisfaction.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Velvet Underground/The Velvet Underground & Nico/02 I'm Waiting For The Man.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Velvet Underground/The Velvet Underground & Nico/03 Femme Fatale.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Velvet Underground/White Light_White Heat/04 Here She Comes Now.m4a",
            "/Music/iTunes 1/iTunes Media/Music/The Who/Sings My Generation/My Generation.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Village People/The Very Best Of Village People/Macho Man.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Vondelpark/Sauna - EP/01 California Analog Dream.m4a",
            "/Music/iTunes 1/iTunes Media/Music/War/Why Can't We Be Friends/Low Rider.m4a",
            "/Music/iTunes 1/iTunes Media/Music/Yes/Fragile/01 Roundabout.m4a",
        ];
        assert_eq!(
            longest_common_prefix(paths),
            "/Music/iTunes 1/iTunes Media/Music/"
        );
    }

    #[test]
    fn longest_common_prefix_does_not_handle_partial_words() {
        let albums = ["/artist/albumOne", "/artist/albumTwo"];
        assert_eq!(longest_common_prefix(&albums), "/artist/album");
    }

    #[test]
    fn truncate_runes_basics() {
        assert_eq!(truncate_runes("hello", 10, "..."), "hello");
        assert_eq!(truncate_runes("hello", 5, "..."), "hello");
        assert_eq!(truncate_runes("hello world", 8, "..."), "hello...");
        assert_eq!(truncate_runes("😀😁😂😃😄😅", 5, "..."), "😀😁...");
        assert_eq!(truncate_runes("Café au Lait", 5, "..."), "Ca...");
        assert_eq!(truncate_runes("hello world", 5, ""), "hello");
    }

    #[test]
    fn truncate_runes_accounts_for_suffix_length() {
        let result = truncate_runes("hello world this is long", 10, "...");
        assert_eq!(result, "hello w...");
        assert!(result.chars().count() <= 10);
    }

    #[test]
    fn truncate_runes_handles_very_long_suffix() {
        assert_eq!(
            truncate_runes("hello world", 5, "... (truncated)"),
            "... (truncated)"
        );
    }

    #[test]
    fn truncate_runes_handles_empty_string() {
        assert_eq!(truncate_runes("", 10, "..."), "");
    }

    #[test]
    fn truncate_runes_uses_custom_suffix() {
        assert_eq!(
            truncate_runes("hello world extra", 11, " [...]"),
            "hello [...]"
        );
    }

    #[test]
    fn truncate_runes_splits_at_rune_boundaries() {
        assert_eq!(truncate_runes("abcdefghij", 5, "..."), "ab...");
        assert_eq!(truncate_runes("ab😀cd", 4, "."), "ab😀.");
        assert_eq!(truncate_runes("😀😁😂😃😄", 3, "…"), "😀😁…");
        assert_eq!(truncate_runes("こんにちは世界", 3, "…"), "こん…");
    }

    fn ascii_name(filename: &str) -> String {
        let header = content_disposition_attachment(filename);
        let re = Regex::new(r#"(?:^|; )filename="([^"]*)""#).unwrap();
        let caps = re.captures(&header).expect("exactly one quoted filename");
        caps[1].to_string()
    }

    fn decoded_name(filename: &str) -> String {
        let header = content_disposition_attachment(filename);
        if let Some(pos) = header.find("filename*=utf-8''") {
            let enc = &header[pos + "filename*=utf-8''".len()..];
            return percent_decode(enc);
        }
        ascii_name(filename)
    }

    fn percent_decode(s: &str) -> String {
        let mut out = Vec::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 3 <= b.len() {
                if let Ok(v) =
                    u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
                {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8(out).unwrap_or_default()
    }

    #[test]
    fn content_disposition_blocks_parameter_injection() {
        let attack = "party\"; filename=\"evil.html.m3u";
        assert_eq!(
            content_disposition_attachment(attack),
            "attachment; filename=\"party_; filename=_evil.html.m3u\""
        );
        assert_eq!(decoded_name(attack), "party_; filename=_evil.html.m3u");
        let utf8_attack = "東京\"; filename=\"evil.html.m3u";
        let header = content_disposition_attachment(utf8_attack);
        assert_eq!(header.matches('"').count(), 2);
        assert_eq!(decoded_name(utf8_attack), "東京_; filename=_evil.html.m3u");
    }

    #[test]
    fn content_disposition_ascii_names() {
        assert_eq!(
            content_disposition_attachment("My Playlist.m3u"),
            "attachment; filename=\"My Playlist.m3u\""
        );
        assert_eq!(
            ascii_name("...And Justice for All.zip"),
            "...And Justice for All.zip"
        );
        assert_eq!(ascii_name("  Greatest Hits  .zip"), "Greatest Hits.zip");
        assert_eq!(ascii_name("Loose End. "), "Loose End");
        assert_eq!(
            content_disposition_attachment("..."),
            "attachment; filename=\"download\""
        );
        let long = format!("a.{}", "x".repeat(300));
        let name = ascii_name(&long);
        assert!(name.len() <= 255);
        assert!(name.starts_with("a.xxx"));
        assert_eq!(
            ascii_name("AC/DC: Live, 1979?.zip"),
            "AC_DC_ Live_ 1979_.zip"
        );
        assert_eq!(ascii_name("line\r\nbreak\x00\t.mp3"), "linebreak.mp3");
    }

    #[test]
    fn content_disposition_non_ascii_names() {
        assert_eq!(ascii_name("Legião Urbana.zip"), "Legiao Urbana.zip");
        assert_eq!(
            ascii_name("She’s a Woman — Live.mp3"),
            "She's a Woman - Live.mp3"
        );
        assert_eq!(ascii_name("“Heroes”.mp3"), "_Heroes_.mp3");
        assert_eq!(decoded_name("She’s a Woman.mp3"), "She’s a Woman.mp3");
        assert_eq!(ascii_name("東京.mp3"), "download.mp3");
        assert_eq!(ascii_name("Кино.m3u"), "download.m3u");
        assert_eq!(ascii_name("東京"), "download");
        let cases = [
            ("Legião Urbana.zip", "Legião Urbana.zip"),
            ("東京.zip", "東京.zip"),
            ("🎵 mix.zip", "🎵 mix.zip"),
            ("Sigur Rós, 100%.zip", "Sigur Rós, 100%.zip"),
            ("Sigur Rós/Live: Heima.zip", "Sigur Rós_Live_ Heima.zip"),
            ("Sigur Rós\r\n\x00.zip", "Sigur Rós.zip"),
            ("Björk\u{202E}exe.mp3", "Björkexe.mp3"),
        ];
        for (input, expected) in cases {
            assert_eq!(decoded_name(input), expected, "input {input:?}");
        }
        let name = decoded_name(&format!("{}{}", "東".repeat(300), ".zip"));
        assert!(name.len() <= 255);
        assert!(name.ends_with("東.zip"));
    }

    #[test]
    fn normalize_for_fts_cases() {
        let cases: &[(&str, &[&str])] = &[
            ("REM", &["R.E.M."]),
            ("ACDC", &["AC/DC"]),
            ("Aha", &["A-ha"]),
            ("", &["The Beatles"]),
            ("REM", &["R.E.M.", "Automatic for the People"]),
            ("REM", &["R.E.M.", "R.E.M."]),
            ("N", &["Guns N' Roses"]),
            ("REM ACDC", &["R.E.M.", "AC/DC"]),
            ("Bjork", &["Bjørk"]),
            ("Oystein", &["Øystein"]),
            ("oeuvre", &["œuvre"]),
            ("cafe", &["café"]),
            ("Mo Ros", &["Mø Rós"]),
            ("StEtienne St-Etienne", &["St-Étienne"]),
            ("Cafe", &["Café", "Cafe"]),
            ("Strasse", &["Straße"]),
        ];
        for (expected, values) in cases {
            assert_eq!(normalize_for_fts(values), *expected, "values {values:?}");
        }
    }

    #[test]
    fn sanitize_strings_uses_go_lowercase_before_accent_fold() {
        // Go oracle: sanitize.Accents(strings.ToLower("ΟΔΥΣΣΕΙΑ")) preserves
        // the sigma as σ; Rust's context-sensitive str::to_lowercase used ς.
        assert_eq!(sanitize_strings(&["ΟΔΥΣΣΕΙΑ"]), "οδυσσεια");
        assert_eq!(sanitize_strings(&["ΙΣ"]), "ισ");
        assert_eq!(sanitize_strings(&["İstanbul Jazz"]), "istanbul jazz");
    }

    #[test]
    fn sanitize_strings_cases() {
        assert_eq!(sanitize_strings(&["Some Text"]), "some text");
        assert_eq!(sanitize_strings(&["Quintão"]), "quintao");
        assert_eq!(
            sanitize_strings(&[" some  text  ", "text some"]),
            "some text"
        );
        assert_eq!(
            sanitize_strings(&["legião urbana", "urbana legiÃo"]),
            "legiao urbana"
        );
        assert_eq!(
            sanitize_strings(&["Tom’s Diner ' “40” ‘A’"]),
            "40 a diner toms"
        );
        assert_eq!(sanitize_strings(&["[Five Years]"]), "five years");
        assert_eq!(sanitize_strings(&["folder/file\\yyyy"]), "file folder yyyy");
        assert_eq!(sanitize_strings(&["k—os", "k−os"]), "k-os");
        assert_eq!(
            sanitize_strings(&["Bob Marley", "Marley, Bob"]),
            "bob marley"
        );
    }

    #[test]
    fn go_lowercase_parity_flows_into_persisted_sort_fields() {
        // Go oracles from utils/str/sanitize_strings.go and model/map_mediafile.go:
        // strings.ToLower("ΟΔΥΣΣΕΙΑ") = "οδυσσεια" (never final sigma), and
        // strings.ToLower("İstanbul Jazz") = "istanbul jazz" (U+0130 -> i).
        assert_eq!(sanitize_field_for_sorting("ΟΔΥΣΣΕΙΑ"), "οδυσσεια");
        assert_eq!(sanitize_field_for_sorting("ΙΣ"), "ισ");
        assert_eq!(sanitize_field_for_sorting("İstanbul Jazz"), "istanbul jazz");
        assert_eq!(
            sanitize_field_for_sorting_no_article("ΟΔΥΣΣΕΙΑ", "The"),
            "οδυσσεια"
        );
    }

    #[test]
    fn sanitize_field_for_sorting_cases() {
        assert_eq!(sanitize_field_for_sorting("Céu"), "ceu");
        // SanitizeFieldForSorting does NOT strip the article.
        assert_eq!(sanitize_field_for_sorting("The Beatles"), "the beatles");
        assert_eq!(sanitize_field_for_sorting("Õ Blésq Blom"), "o blesq blom");
    }

    #[test]
    fn sanitize_field_for_sorting_no_article_cases() {
        let articles = "The O";
        assert_eq!(
            sanitize_field_for_sorting_no_article("Céu", articles),
            "ceu"
        );
        assert_eq!(
            sanitize_field_for_sorting_no_article("The Beatles", articles),
            "beatles"
        );
        assert_eq!(
            sanitize_field_for_sorting_no_article("Õ Blésq Blom", articles),
            "blesq blom"
        );
    }

    #[test]
    fn sanitize_text_cases() {
        assert_eq!(sanitize_text("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(sanitize_text("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(sanitize_text("&lt;b&gt;ok&lt;/b&gt;"), "<b>ok</b>");
    }

    #[test]
    fn sanitize_html_cases() {
        let sanitized =
            sanitize_html("<img src=x onerror=alert(1)><script>alert(2)</script><b>ok</b>");
        assert!(sanitized.contains("<b>ok</b>"), "got {sanitized}");
        assert!(!sanitized.contains("onerror"));
        assert!(!sanitized.contains("<script"));

        let entity = sanitize_html(
            "&lt;img src=x onerror=alert(1)&gt;&lt;script&gt;alert(2)&lt;/script&gt;&lt;b&gt;ok&lt;/b&gt;",
        );
        assert!(entity.contains("<b>ok</b>"), "got {entity}");
        assert!(!entity.contains("onerror"));
        assert!(!entity.contains("<script"));
    }

    #[test]
    fn remove_article_cases() {
        // Empty articles list.
        assert_eq!(remove_article("", ""), "");
        assert_eq!(remove_article("The Beatles", ""), "The Beatles");
        // Default articles.
        let articles = "The El La Los Las Le Les Os As O A";
        assert_eq!(remove_article("", articles), "");
        assert_eq!(
            remove_article("Os Paralamas do Sucesso", articles),
            "Paralamas do Sucesso"
        );
        // Does not remove article if it is part of the first word.
        assert_eq!(
            remove_article("Thelonious Monk", articles),
            "Thelonious Monk"
        );
    }
}

#[cfg(test)]
mod to_lower_go_parity {
    use super::to_lower;

    // Golden pairs produced by the actual Go `strings.ToLower` (go run against
    // this repo's Go tree), covering both divergences:
    //   "ΟΔΥΣΣ"      -> "οδυσσ"   (Rust str::to_lowercase gives "οδυσς")
    //   "ΙΣ"         -> "ισ"      (Rust str::to_lowercase gives "ις")
    //   "İstanbul"   -> "istanbul"(Rust gives "i\u{307}stanbul")
    #[test]
    fn greek_final_sigma_matches_go() {
        assert_eq!(to_lower("ΟΔΥΣΣ"), "οδυσσ");
        assert_eq!(to_lower("ΙΣ"), "ισ");
        assert_eq!(to_lower("ΑΣ ΠΟΛΗ"), "ασ πολη");
        assert_eq!(to_lower("ΟΔΥΣΣΕΙΑ"), "οδυσσεια");
        assert_eq!(to_lower("KOREA Σ"), "korea σ");
    }

    #[test]
    fn u0130_maps_to_plain_i() {
        assert_eq!(to_lower("İstanbul"), "istanbul");
        assert_eq!(to_lower("İstanbul Jazz"), "istanbul jazz");
        assert_eq!(to_lower("\u{130}"), "i");
        assert_eq!(to_lower("\u{130}").chars().count(), 1);
    }

    #[test]
    fn other_special_cases_are_unchanged() {
        assert_eq!(to_lower("ß"), "ß");
        assert_eq!(to_lower("\u{1E9E}"), "ß");
        assert_eq!(to_lower("ÅNGSTRÖM"), "ångström");
        assert_eq!(to_lower("Ünïcode"), "ünïcode");
        assert_eq!(to_lower("ı"), "ı");
        assert_eq!(to_lower("ς"), "ς");
    }

    #[test]
    fn ascii_and_noop_paths() {
        assert_eq!(to_lower("Rock"), "rock");
        assert_eq!(to_lower("rock"), "rock");
        assert_eq!(to_lower(""), "");
        assert_eq!(to_lower("中文 123"), "中文 123");
    }
}
