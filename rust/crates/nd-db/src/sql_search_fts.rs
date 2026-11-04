//! Port of `persistence/sql_search_fts.go` — the FTS5 search strategy.
//!
//! [`build_fts5_query`] preprocesses user input into a safe FTS5 MATCH
//! expression (Go `buildFTS5Query`, sql_search_fts.go:108). Everything the
//! user typed is reduced to FTS5 query syntax characters (letters, numbers,
//! `*`, `"` and internal `\0` phrase placeholders) before it ever reaches
//! SQL, and the final expression is only ever bound as a **parameter value**
//! — never interpolated into SQL text.
//!
//! [`new_fts_search`] is Go `newFTSSearch` (sql_search_fts.go:331): it builds
//! the FTS strategy or signals the LIKE fallback. Executing the LIKE branch is
//! owned by the `sql_search_like` lane; this module only reports it.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::sqlrepo::{Repository, Value};

/// Characters kept by the sanitizer (Go `fts5SpecialChars`,
/// sql_search_fts.go:38): Unicode letters, numbers, whitespace, `*` (prefix
/// wildcard), `"` (phrase quotes) and `\0` (internal placeholder marker).
/// All other characters are replaced with a space because the unicode61
/// tokenizer treats them as token separators, and characters like `'` cause
/// FTS5 parse errors as unbalanced string delimiters.
fn is_sanitizer_kept(c: char) -> bool {
    c.is_alphanumeric() || c.is_whitespace() || c == '*' || c == '"' || c == '\0'
}

/// Go's `\x00PHRASE<i>\x00` placeholder (sql_search_fts.go:87,119).
fn phrase_placeholder(index: usize) -> String {
    format!("\u{0}PHRASE{index}\u{0}")
}

/// Transliteration table of `sanitize.Accents` (deluan/sanitize sanitize.go:218)
/// — the exact map used by Go at sql_search_fts.go:131.
fn transliteration(c: char) -> Option<&'static str> {
    match c {
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
        'Ù' | 'Ú' | 'Û' | 'Ü' => Some("U"),
        'Ý' => Some("Y"),
        'Þ' => Some("TH"),
        'ẞ' => Some("SS"),
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
        'þ' => Some("th"),
        'ß' => Some("ss"),
        _ => None,
    }
}

/// Port of `sanitize.Accents` (sql_search_fts.go:131): transliterate
/// non-ASCII letters (ø→o, æ→ae, œ→oe, ß→ss, …) so the query matches the
/// ASCII variants emitted by normalizeForFTS at index time. FTS5's own
/// `remove_diacritics 2` only strips NFKD-decomposable marks.
fn sanitize_accents(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if let Some(replacement) = transliteration(c) {
            out.push_str(replacement);
        } else {
            out.push(c);
        }
    }
    out
}

/// Port of `fts5Operators` (`(?i)\b(AND|OR|NOT|NEAR)\b`,
/// sql_search_fts.go:41): neutralize FTS5 boolean operators by lowercasing
/// them as whole words — FTS5 operators are case-sensitive, so `and`, `or`,
/// `not`, `near` are plain tokens. `\b` in Go regexp is an ASCII word
/// boundary (`[A-Za-z0-9_]`).
fn neutralize_operators(s: &str) -> String {
    const OPERATORS: [&str; 4] = ["AND", "OR", "NOT", "NEAR"];
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let mut matched = false;
        for op in OPERATORS {
            let op_bytes = op.as_bytes();
            let end = i + op_bytes.len();
            if end <= bytes.len()
                && bytes[i..end].eq_ignore_ascii_case(op_bytes)
                && (i == 0 || !is_word_byte(bytes[i - 1]))
                && (end == bytes.len() || !is_word_byte(bytes[end]))
            {
                out.push_str(&s[i..end].to_ascii_lowercase());
                i = end;
                matched = true;
                break;
            }
        }
        if !matched {
            let c = s[i..].chars().next().unwrap_or('\0');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// Port of `isSingleUnicodeLetter` (sql_search_fts.go:50): the token is
/// exactly one Unicode letter.
fn is_single_unicode_letter(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next().is_some_and(char::is_alphabetic) && chars.next().is_none()
}

/// Port of `isDottedAbbreviation` (sql_search_fts.go:96): true if the word
/// uses only dots as punctuation and all sub-tokens are single letters
/// (e.g. "R.E.M.", "U.K." but not "a-ha" or "AC/DC").
fn is_dotted_abbreviation(word: &str, sub_tokens: &[&str]) -> bool {
    word.chars()
        .all(|c| c.is_alphabetic() || c.is_numeric() || c == '.')
        && sub_tokens.iter().all(|t| is_single_unicode_letter(t))
}

/// Port of `processPunctuatedWords` (sql_search_fts.go:65): handle words
/// with embedded name punctuation (`-/.'`) before the general
/// special-character strip, producing either a quoted phrase for dotted
/// abbreviations (R.E.M. → `"R E M"`) or a phrase+concat OR for other
/// patterns (a-ha → `("a ha" OR aha*)`).
fn process_punctuated_words(input: &str, phrases: &mut Vec<String>) -> String {
    const NAME_PUNCTUATION: [char; 4] = ['-', '/', '.', '\''];
    let mut result = Vec::new();
    for word in input.split_whitespace() {
        if word.starts_with('\0')
            || word.contains('*')
            || word.contains('"')
            || !word.chars().any(|c| NAME_PUNCTUATION.contains(&c))
        {
            result.push(word.to_string());
            continue;
        }
        let concat: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if concat.is_empty() || concat == word {
            result.push(word.to_string());
            continue;
        }
        let stripped: String = word
            .chars()
            .map(|c| if is_sanitizer_kept(c) { c } else { ' ' })
            .collect();
        let sub_tokens: Vec<&str> = stripped.split_whitespace().collect();
        if sub_tokens.len() < 2 {
            // Single sub-token after splitting (e.g. N' → N): stripped form.
            result.push(concat);
            continue;
        }
        if is_dotted_abbreviation(word, &sub_tokens) {
            phrases.push(format!("\"{}\"", sub_tokens.join(" ")));
        } else {
            phrases.push(format!("(\"{}\" OR {}*)", sub_tokens.join(" "), concat));
        }
        result.push(phrase_placeholder(phrases.len() - 1));
    }
    result.join(" ")
}

/// Port of `ftsQueryDegraded` (sql_search_fts.go:280): true when FTS
/// tokenization stripped significant content from the query (e.g. "1+" →
/// `1*`), leaving only very short/broad tokens. Lengths are byte lengths,
/// matching Go's `len(string)`.
fn fts_query_degraded(original: &str, fts_query: &str) -> bool {
    let original = original.trim();
    if original.is_empty() || fts_query.is_empty() {
        return false;
    }
    // Strip quotes from original — compare against the raw content.
    let stripped = original.replace('"', "");
    // If the original is entirely alphanumeric, nothing was stripped.
    if stripped.chars().all(|c| c.is_alphanumeric()) {
        return false;
    }
    for token in fts_query.split_whitespace() {
        let token = token.strip_suffix('*').unwrap_or(token);
        // Skip internal phrase placeholders.
        if token.starts_with('\0') {
            return false;
        }
        // OR groups from process_punctuated_words were handled meaningfully.
        if token.starts_with('(') {
            return false;
        }
        // Quoted phrases: check the tokens inside as FTS5 will tokenize them.
        if token.starts_with('"') {
            let inner = token.trim_matches('"');
            let inner_alpha: String = inner
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                .collect();
            for inner_token in inner_alpha.split_whitespace() {
                if inner_token.len() > 2 {
                    return false;
                }
            }
            continue;
        }
        if token.len() > 2 {
            return false;
        }
    }
    true
}

/// Port of `buildFTS5Query` (sql_search_fts.go:108): preprocess user input
/// into a safe FTS5 MATCH expression. Plain tokens are emitted as
/// `(token OR token*)` so bm25 ranks exact-token hits above prefix-only
/// matches; quoted phrases and `*` prefix wildcards pass through; FTS5
/// operators are neutralized by lowercasing; all other special characters
/// are stripped to prevent query injection. The output is a MATCH
/// expression, which callers must bind as a parameter value — never
/// interpolate into SQL.
///
/// The bool reports whether tokenization degraded the query (see
/// [`fts_query_degraded`]).
#[must_use]
pub fn build_fts5_query(user_input: &str) -> (String, bool) {
    let q = user_input.trim();
    if q.is_empty() || q == "\"\"" {
        return (String::new(), false);
    }

    // Extract quoted phrases into \0PHRASE<i>\0 placeholders so accents,
    // operator neutralization and the character strip leave them verbatim.
    let mut phrases: Vec<String> = Vec::new();
    let mut result = q.to_string();
    while let Some(start) = result.find('"') {
        let Some(relative_end) = result[start + 1..].find('"') else {
            // Unmatched quote — remove it.
            result.remove(start);
            break;
        };
        let end = start + 1 + relative_end;
        phrases.push(result[start..end + 1].to_string());
        result = format!(
            "{}{}{}",
            &result[..start],
            phrase_placeholder(phrases.len() - 1),
            &result[end + 1..]
        );
    }

    // Transliterate non-ASCII letters in the unquoted portion.
    result = sanitize_accents(&result);

    // Neutralize FTS5 operators by lowercasing them (case-sensitive in FTS5).
    result = neutralize_operators(&result);

    // Handle words with embedded punctuation (a-ha, AC/DC, R.E.M.) first.
    result = process_punctuated_words(&result, &mut phrases);

    // Strip everything the sanitizer does not keep.
    let stripped: String = result
        .chars()
        .map(|c| if is_sanitizer_kept(c) { c } else { ' ' })
        .collect();

    // FTS5 only supports * at the end of a token (prefix queries).
    let mut prefix_tokens: Vec<String> = Vec::new();
    let mut wrapped_tokens: Vec<String> = Vec::new();
    for token in stripped.split_whitespace() {
        // fts5LeadingStar (sql_search_fts.go:43): a leading * is dropped; a
        // token that becomes empty (the standalone-"*" case) yields no token.
        let stripped_star = token.strip_prefix('*').unwrap_or(token);
        if stripped_star.is_empty() {
            continue;
        }
        let token = stripped_star;
        if token.starts_with('\0') || token.ends_with('*') {
            // Placeholders and user-supplied wildcards pass through untouched.
            prefix_tokens.push(token.to_string());
            wrapped_tokens.push(token.to_string());
            continue;
        }
        prefix_tokens.push(format!("{token}*"));
        // The OR adds no matches (exact ⊂ prefix) but gives bm25 a high-IDF
        // exact-term hit, ranking rows that contain the literal word above
        // prefix-only matches.
        wrapped_tokens.push(format!("({token} OR {token}*)"));
    }

    // Explicit AND between tokens — FTS5's implicit AND (space-separated)
    // doesn't work correctly with parenthesized OR groups. The prefix form
    // is space-joined: it only feeds fts_query_degraded.
    let mut prefix_query = prefix_tokens.join(" ");
    let mut final_query = wrapped_tokens.join(" AND ");

    for (index, phrase) in phrases.iter().enumerate() {
        let placeholder = phrase_placeholder(index);
        prefix_query = prefix_query.replace(&placeholder, phrase);
        final_query = final_query.replace(&placeholder, phrase);
    }

    (final_query, fts_query_degraded(user_input, &prefix_query))
}

/// Port of `containsCJK` (sql_search_fts.go:18): true if the string contains
/// any CJK (Han/Hiragana/Katakana/Hangul) character. CJK text doesn't use
/// spaces, so FTS5's unicode61 tokenizer treats entire CJK phrases as single
/// tokens, making token-based search ineffective for CJK content.
#[must_use]
pub fn contains_cjk(s: &str) -> bool {
    const CJK_RANGES: [(u32, u32); 10] = [
        (0x2E80, 0x2FDF), // CJK Radicals / Kangxi
        (0x3005, 0x3007), // Ideographic iteration mark
        (0x3021, 0x3029), // Hangzhou numerals
        (0x3038, 0x303B), // Hangzhou numerals ten..thirty
        (0x3400, 0x4DBF), // CJK Unified Ideographs Extension A
        (0x4E00, 0x9FFF), // CJK Unified Ideographs
        (0xF900, 0xFAFF), // CJK Compatibility Ideographs
        (0x3041, 0x30FF), // Hiragana + Katakana
        (0x31F0, 0x31FF), // Katakana Phonetic Extensions
        (0xAC00, 0xD7AF), // Hangul Syllables
    ];
    s.chars().any(|c| {
        let code = c as u32;
        CJK_RANGES
            .iter()
            .any(|(lo, hi)| (*lo..=*hi).contains(&code))
    })
}

/// Port of `qualifyOrderBy` (sql_search_fts.go:246): prepend `table_name` to
/// a simple column name. Returns `None` for complex expressions (function
/// calls, aggregations) that can't be used in the FTS phase-1 query.
#[must_use]
pub fn qualify_order_by(table_name: &str, order_by: &str) -> Option<String> {
    let order_by = order_by.trim();
    if order_by.is_empty() || order_by.contains('(') || order_by.contains(',') {
        return None;
    }
    let mut parts: Vec<String> = order_by.split_whitespace().map(String::from).collect();
    if let Some(first) = parts.first_mut() {
        if !first.contains('.') {
            *first = format!("{table_name}.{first}");
        }
    }
    Some(parts.join(" "))
}

/// One FTS5 column with its BM25 relevance weight (Go `ftsColumn`,
/// sql_search_fts.go:153).
struct FtsColumn {
    name: &'static str,
    weight: f64,
}

/// Port of `ftsColumnDefs` (sql_search_fts.go:162): FTS5 columns and their
/// BM25 weights. The order MUST match the FTS5 table column order.
fn fts_column_defs(table: &str) -> Option<&'static [FtsColumn]> {
    match table {
        "media_file" => Some(&[
            FtsColumn {
                name: "title",
                weight: 10.0,
            },
            FtsColumn {
                name: "album",
                weight: 5.0,
            },
            FtsColumn {
                name: "artist",
                weight: 3.0,
            },
            FtsColumn {
                name: "album_artist",
                weight: 3.0,
            },
            FtsColumn {
                name: "sort_title",
                weight: 1.0,
            },
            FtsColumn {
                name: "sort_album_name",
                weight: 1.0,
            },
            FtsColumn {
                name: "sort_artist_name",
                weight: 1.0,
            },
            FtsColumn {
                name: "sort_album_artist_name",
                weight: 1.0,
            },
            FtsColumn {
                name: "disc_subtitle",
                weight: 1.0,
            },
            FtsColumn {
                name: "search_participants",
                weight: 2.0,
            },
            FtsColumn {
                name: "search_normalized",
                weight: 1.0,
            },
        ]),
        "album" => Some(&[
            FtsColumn {
                name: "name",
                weight: 10.0,
            },
            FtsColumn {
                name: "sort_album_name",
                weight: 1.0,
            },
            FtsColumn {
                name: "album_artist",
                weight: 3.0,
            },
            FtsColumn {
                name: "search_participants",
                weight: 2.0,
            },
            FtsColumn {
                name: "discs",
                weight: 1.0,
            },
            FtsColumn {
                name: "catalog_num",
                weight: 1.0,
            },
            FtsColumn {
                name: "album_version",
                weight: 1.0,
            },
            FtsColumn {
                name: "search_normalized",
                weight: 1.0,
            },
        ]),
        "artist" => Some(&[
            FtsColumn {
                name: "name",
                weight: 10.0,
            },
            FtsColumn {
                name: "sort_artist_name",
                weight: 1.0,
            },
            // Same weight as name: for artists this column is purely the name
            // in alternate spelling (unlike media_file/album, where it mixes
            // variants and full weight would distort ranking).
            FtsColumn {
                name: "search_normalized",
                weight: 10.0,
            },
        ]),
        _ => None,
    }
}

/// Precomputed per-table FTS5 config (Go's `init()`-built
/// `ftsColumnFilters`/`ftsBM25Weights`, sql_search_fts.go:203-215).
struct FtsConfig {
    /// `{col1 col2 …}` column filter for the MATCH expression, containing
    /// only columns with weight > 0.
    column_filter: String,
    /// `"10.0, 5.0, …"` weight CSV for the `bm25()` rank expression.
    bm25_weights: String,
}

static FTS_CONFIGS: LazyLock<HashMap<&'static str, FtsConfig>> = LazyLock::new(|| {
    let mut configs = HashMap::new();
    for table in ["media_file", "album", "artist"] {
        let cols = fts_column_defs(table).unwrap_or(&[]);
        let names = cols
            .iter()
            .filter(|c| c.weight > 0.0)
            .map(|c| c.name)
            .collect::<Vec<_>>()
            .join(" ");
        let weights = cols
            .iter()
            .map(|c| format!("{:.1}", c.weight))
            .collect::<Vec<_>>()
            .join(", ");
        configs.insert(
            table,
            FtsConfig {
                column_filter: format!("{{{names}}}"),
                bm25_weights: weights,
            },
        );
    }
    configs
});

/// Port of the `ftsColumnFilters` lookup (sql_search_fts.go:210,336): the
/// `{…}` column filter for a known table, or `None` for unknown tables.
#[must_use]
pub fn fts_column_filter(table_name: &str) -> Option<String> {
    FTS_CONFIGS.get(table_name).map(|c| c.column_filter.clone())
}

/// Port of the `ftsBM25Weights` lookup (sql_search_fts.go:212,337): the
/// bm25 weight CSV for a known table, or `None` for unknown tables.
#[must_use]
pub fn fts_bm25_weights(table_name: &str) -> Option<String> {
    FTS_CONFIGS.get(table_name).map(|c| c.bm25_weights.clone())
}

/// Port of Go `ftsSearch` (sql_search_fts.go:222): the FTS5 search strategy.
/// `match_expr` is the sanitized FTS5 MATCH expression derived from user
/// input — it must always be bound as a parameter value, never interpolated
/// into SQL text.
#[derive(Debug, Clone, PartialEq)]
pub struct FtsSearch {
    /// The base table being searched (e.g. `media_file`).
    pub table_name: String,
    /// The FTS5 shadow table (`<table_name>_fts`).
    pub fts_table: String,
    /// The sanitized FTS5 MATCH expression (parameter value, never SQL text).
    pub match_expr: String,
    /// The rank expression (`bm25(fts, weights)` or `fts.rank`).
    pub rank_expr: String,
}

impl FtsSearch {
    /// Port of `ftsSearch.ToSql` (sql_search_fts.go:228): a single-query
    /// fallback for the REST filter path (no two-phase split). The user's
    /// match expression is bound as the sole `?` parameter.
    ///
    /// Apply the returned fragment with [`Repository::where_raw`] for both
    /// the select and the count paths, mirroring Go's use of this fragment
    /// in `toSQL` for `getAll`/`count`.
    #[must_use]
    pub fn to_sql(&self) -> (String, Vec<Value>) {
        let sql = format!(
            "{}.rowid IN (SELECT rowid FROM {} WHERE {} MATCH ?)",
            self.table_name, self.fts_table, self.fts_table
        );
        (sql, vec![Value::Text(self.match_expr.clone())])
    }

    /// Port of the phase-1 `rowidCore` select in `ftsSearch.execute`
    /// (sql_search_fts.go:233-240): the base table's rowids joined to the
    /// FTS5 table, ordered by the BM25 rank expression first, then the
    /// caller's qualified order-by expressions. The user's match expression
    /// is bound at the join position as a parameter. Complex order-by
    /// expressions (function calls, aggregations) are dropped from phase 1,
    /// matching Go.
    #[must_use]
    pub fn rowid_select(&self, order_by: &[String]) -> Repository {
        let mut qualified_order_bys = vec![self.rank_expr.clone()];
        for ob in order_by {
            if let Some(qualified) = qualify_order_by(&self.table_name, ob) {
                qualified_order_bys.push(qualified);
            }
        }
        let join_on = format!(
            "{}.rowid = {}.rowid AND {} MATCH ?",
            self.fts_table, self.table_name, self.fts_table
        );
        Repository::new(self.table_name.clone())
            .select([format!("{}.rowid", self.table_name)])
            .join(
                self.fts_table.clone(),
                join_on,
                vec![Value::Text(self.match_expr.clone())],
            )
            .order_by(qualified_order_bys)
    }
}

/// Port of the `searchStrategy` return of `newFTSSearch`
/// (sql_search_fts.go:331): the chosen strategy for a user query — FTS5, or
/// the LIKE fallback with the cleaned query. Executing the LIKE branch is
/// owned by the `sql_search_like` module; this enum only reports it.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchStrategy {
    /// Full-text search via FTS5 with BM25 ranking.
    Fts(FtsSearch),
    /// LIKE fallback for queries that FTS tokenization cannot serve
    /// (punctuation-only like `!!!!!!!`, or degraded like `1+`). Carries the
    /// cleaned query (original minus quotes, trimmed).
    Like(String),
}

/// Port of `newFTSSearch` (sql_search_fts.go:331): build the FTS5 search
/// strategy for `query`, falling back to LIKE when the query produces no FTS
/// tokens or FTS tokenization stripped significant content from it. Returns
/// `None` when the query produces no searchable tokens at all (Go's nil).
#[must_use]
pub fn new_fts_search(table_name: &str, query: &str) -> Option<SearchStrategy> {
    let (q, degraded) = build_fts5_query(query);
    if q.is_empty() || degraded {
        let cleaned = query.trim().replace('"', "");
        if !cleaned.is_empty() {
            return Some(SearchStrategy::Like(cleaned));
        }
        return None;
    }
    let fts_table = format!("{table_name}_fts");
    let match_expr = match fts_column_filter(table_name) {
        Some(filter) => format!("{filter} : ({q})"),
        None => q,
    };
    let rank_expr = match fts_bm25_weights(table_name) {
        Some(weights) => format!("bm25({fts_table}, {weights})"),
        None => format!("{fts_table}.rank"),
    };
    Some(SearchStrategy::Fts(FtsSearch {
        table_name: table_name.to_string(),
        fts_table,
        match_expr,
        rank_expr,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_words_are_wrapped_as_exact_or_prefix() {
        // Arrange: Go table "wraps a single word as exact OR prefix" and
        // "wraps each word as exact OR prefix".
        let cases = [
            ("beatles", "(beatles OR beatles*)"),
            ("abbey road", "(abbey OR abbey*) AND (road OR road*)"),
            ("abbey   road", "(abbey OR abbey*) AND (road OR road*)"),
        ];
        for (input, expected) in cases {
            // Act:
            let (q, _) = build_fts5_query(input);
            // Assert:
            assert_eq!(q, expected, "input {input:?}");
        }
    }

    #[test]
    fn empty_inputs_return_empty_query() {
        // Arrange: Go entries for empty/whitespace/punctuation-only input.
        let cases = ["", "   ", "!!!!!!!", "!@#$%^&", "\"\""];
        for input in cases {
            // Act:
            let (q, degraded) = build_fts5_query(input);
            // Assert:
            assert_eq!(q, "", "input {input:?}");
            assert!(!degraded, "input {input:?}");
        }
    }

    #[test]
    fn quoted_phrases_are_preserved_verbatim() {
        // Arrange: Go "preserves quoted phrases without wrapping" and mixed
        // phrase/word entries; quoted unicode and punctuation stay verbatim.
        let cases = [
            ("\"the beatles\"", "\"the beatles\""),
            (
                "\"the beatles\" abbey",
                "\"the beatles\" AND (abbey OR abbey*)",
            ),
            ("\"Björk\"", "\"Björk\""),
            ("\"ac/dc\"", "\"ac/dc\""),
            ("\"R.E.M.\"", "\"R.E.M.\""),
        ];
        for (input, expected) in cases {
            // Act:
            let (q, _) = build_fts5_query(input);
            // Assert:
            assert_eq!(q, expected, "input {input:?}");
        }
    }

    #[test]
    fn leading_operators_are_neutralized() {
        // Arrange: Go "strips FTS5 operators and wraps lowercased words".
        let input = "AND OR NOT NEAR";
        // Act:
        let (q, _) = build_fts5_query(input);
        // Assert:
        assert_eq!(
            q,
            "(and OR and*) AND (or OR or*) AND (not OR not*) AND (near OR near*)"
        );
    }

    #[test]
    fn unicode_and_accented_input_is_transliterated() {
        // Arrange: Go transliteration entries (sanitize.Accents semantics).
        let cases = [
            ("Björk début", "(Bjork OR Bjork*) AND (debut OR debut*)"),
            ("Øystein", "(Oystein OR Oystein*)"),
            ("œuvre", "(oeuvre OR oeuvre*)"),
            ("Brennæ", "(Brennae OR Brennae*)"),
            (
                "Mø Sigur Rós",
                "(Mo OR Mo*) AND (Sigur OR Sigur*) AND (Ros OR Ros*)",
            ),
            ("Straße", "(Strasse OR Strasse*)"),
        ];
        for (input, expected) in cases {
            // Act:
            let (q, _) = build_fts5_query(input);
            // Assert:
            assert_eq!(q, expected, "input {input:?}");
        }
    }

    #[test]
    fn punctuated_words_must_be_quoted() {
        // Arrange: Go punctuated-word entries — these must come out as
        // quoted phrases (or phrase+concat OR groups), never raw punctuation.
        let cases = [
            ("AC/DC", "(\"AC DC\" OR ACDC*)"),
            ("a-ha", "(\"a ha\" OR aha*)"),
            ("a-h", "(\"a h\" OR ah*)"),
            ("Jay-Z", "(\"Jay Z\" OR JayZ*)"),
            ("it's", "(\"it s\" OR its*)"),
            (
                "best of a-ha",
                "(best OR best*) AND (of OR of*) AND (\"a ha\" OR aha*)",
            ),
            ("you've got", "(\"you ve\" OR youve*) AND (got OR got*)"),
            ("R.E.M.", "\"R E M\""),
            ("R.E.M", "\"R E M\""),
            (
                "best of R.E.M.",
                "(best OR best*) AND (of OR of*) AND \"R E M\"",
            ),
            ("U.K.", "\"U K\""),
        ];
        for (input, expected) in cases {
            // Act:
            let (q, _) = build_fts5_query(input);
            // Assert:
            assert_eq!(q, expected, "input {input:?}");
        }
    }

    #[test]
    fn punctuation_and_apostrophes_are_stripped_or_quoted() {
        // Arrange: Go entries for special FTS5 syntax chars, apostrophes and
        // standalone single letters.
        let cases = [
            (
                "test^col:val",
                "(test OR test*) AND (col OR col*) AND (val OR val*)",
            ),
            (
                "rock & roll, vol. 2",
                "(rock OR rock*) AND (roll OR roll*) AND (vol OR vol*) AND (2 OR 2*)",
            ),
            (
                "Guns N' Roses",
                "(Guns OR Guns*) AND (N OR N*) AND (Roses OR Roses*)",
            ),
            ("I am fine", "(I OR I*) AND (am OR am*) AND (fine OR fine*)"),
            ("A test", "(A OR A*) AND (test OR test*)"),
        ];
        for (input, expected) in cases {
            // Act:
            let (q, _) = build_fts5_query(input);
            // Assert:
            assert_eq!(q, expected, "input {input:?}");
        }
    }

    #[test]
    fn wildcards_pass_through_and_leading_stars_strip() {
        // Arrange: Go wildcard entries.
        let cases = [
            ("beat*", "beat*"),
            ("*livia", "(livia OR livia*)"),
            ("*livia oliv*", "(livia OR livia*) AND oliv*"),
            ("beat* abbey", "beat* AND (abbey OR abbey*)"),
            ("*", ""),
        ];
        for (input, expected) in cases {
            // Act:
            let (q, _) = build_fts5_query(input);
            // Assert:
            assert_eq!(q, expected, "input {input:?}");
        }
    }

    #[test]
    fn degraded_flag_matches_go_table() {
        // Arrange: Go "buildFTS5Query degraded flag" entries.
        let cases = [
            ("beatles", false),
            ("1+", true),
            ("1+ 2+", true),
            ("1+ beatles", false),
            ("\"1+\"", true),
            ("AC/DC", false),
            ("", false),
        ];
        for (input, expected) in cases {
            // Act:
            let (_, degraded) = build_fts5_query(input);
            // Assert:
            assert_eq!(degraded, expected, "input {input:?}");
        }
    }

    #[test]
    fn fts_query_degraded_matches_go_table() {
        // Arrange: Go "ftsQueryDegraded" entries.
        let cases = [
            ("", "1*", false),
            ("1+", "", false),
            ("beatles", "beatles*", false),
            ("test^val", "test* val*", false),
            ("\"the beatles\"", "\"the beatles\"", false),
            ("\"1+\"", "\"1+\"", true),
            ("\"C++ programming\"", "\"C++ programming\"", false),
            ("1+", "1*", true),
            ("C# 1", "C* 1*", true),
            ("1+ beatles", "1* beatles*", false),
            ("AC/DC", "(\"AC DC\" OR ACDC*)", false),
        ];
        for (original, fts_query, expected) in cases {
            // Act:
            let degraded = fts_query_degraded(original, fts_query);
            // Assert:
            assert_eq!(degraded, expected, "original {original:?}");
        }
    }

    #[test]
    fn contains_cjk_matches_go_table() {
        // Arrange: Go "containsCJK" entries.
        let cases = [
            ("", false),
            ("hello world", false),
            ("Björk début", false),
            ("周杰伦", true),
            ("こんにちは", true),
            ("カタカナ", true),
            ("한국어", true),
            ("best of 周杰伦", true),
            ("a曲b", true),
        ];
        for (input, expected) in cases {
            // Act:
            let actual = contains_cjk(input);
            // Assert:
            assert_eq!(actual, expected, "input {input:?}");
        }
    }

    #[test]
    fn qualify_order_by_matches_go_table() {
        // Arrange: Go "qualifyOrderBy" entries.
        let cases = [
            ("artist", "", ""),
            ("artist", "name", "artist.name"),
            ("artist", "name desc", "artist.name desc"),
            ("artist", "artist.name", "artist.name"),
            ("artist", "artist.name desc", "artist.name desc"),
            ("artist", "sum(json_extract(stats, '$.total.m')) desc", ""),
            ("artist", "a, b", ""),
            ("media_file", "title", "media_file.title"),
        ];
        for (table, order_by, expected) in cases {
            // Act:
            let qualified = qualify_order_by(table, order_by);
            // Assert:
            if expected.is_empty() {
                assert!(qualified.is_none(), "order_by {order_by:?}");
            } else {
                assert_eq!(
                    qualified.unwrap_or_default(),
                    expected,
                    "order_by {order_by:?}"
                );
            }
        }
    }

    #[test]
    fn fts_column_filters_match_go_precomputed_strings() {
        // Arrange: Go "ftsColumnFilters" expectations.
        // Act / Assert:
        assert_eq!(
            fts_column_filter("media_file").unwrap_or_default(),
            "{title album artist album_artist sort_title sort_album_name sort_artist_name \
             sort_album_artist_name disc_subtitle search_participants search_normalized}"
        );
        assert_eq!(
            fts_column_filter("album").unwrap_or_default(),
            "{name sort_album_name album_artist search_participants discs catalog_num \
             album_version search_normalized}"
        );
        assert_eq!(
            fts_column_filter("artist").unwrap_or_default(),
            "{name sort_artist_name search_normalized}"
        );
        assert!(fts_column_filter("unknown").is_none());
    }

    #[test]
    fn fts_bm25_weights_match_go_precomputed_strings() {
        // Arrange: Go "ftsBM25Weights" expectations.
        // Act / Assert:
        assert_eq!(
            fts_bm25_weights("media_file").unwrap_or_default(),
            "10.0, 5.0, 3.0, 3.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 1.0"
        );
        assert_eq!(
            fts_bm25_weights("album").unwrap_or_default(),
            "10.0, 1.0, 3.0, 2.0, 1.0, 1.0, 1.0, 1.0"
        );
        assert_eq!(
            fts_bm25_weights("artist").unwrap_or_default(),
            "10.0, 1.0, 10.0"
        );
        assert!(fts_bm25_weights("unknown").is_none());
    }

    #[test]
    fn new_fts_search_builds_strategy_or_falls_back() {
        // Arrange: Go "newFTSSearch" entries.
        // Act / Assert:
        assert!(new_fts_search("media_file", "").is_none());
        assert!(new_fts_search("media_file", "   ").is_none());
        assert!(new_fts_search("media_file", "\"\"").is_none());

        // Punctuation-only and degraded queries fall back to LIKE.
        assert_eq!(
            new_fts_search("media_file", "!!!!!!!"),
            Some(SearchStrategy::Like("!!!!!!!".to_string()))
        );
        assert_eq!(
            new_fts_search("album", "1+"),
            Some(SearchStrategy::Like("1+".to_string()))
        );

        // Single-character queries are not rejected here (min length is
        // enforced in the search orchestration, not in the strategy).
        assert!(new_fts_search("media_file", "a").is_some());
    }

    #[test]
    fn new_fts_search_wraps_match_expr_with_column_filter() {
        // Arrange: Go "wraps query with column filter" and
        // "passes query without column filter for unknown tables".
        // Act:
        let strategy = new_fts_search("artist", "Beatles");
        // Assert:
        match strategy {
            Some(SearchStrategy::Fts(fts)) => {
                assert_eq!(
                    fts.match_expr,
                    "{name sort_artist_name search_normalized} : ((Beatles OR Beatles*))"
                );
                assert_eq!(fts.table_name, "artist");
                assert_eq!(fts.fts_table, "artist_fts");
                assert!(fts.rank_expr.starts_with("bm25(artist_fts,"));
                assert!(fts.rank_expr.contains("10.0"));
            }
            other => panic!("expected Fts, got {other:?}"),
        }

        // Act: unknown table — no filter, ftsTable.rank fallback.
        let strategy = new_fts_search("unknown_table", "test");
        // Assert:
        match strategy {
            Some(SearchStrategy::Fts(fts)) => {
                assert_eq!(fts.match_expr, "(test OR test*)");
                assert_eq!(fts.rank_expr, "unknown_table_fts.rank");
            }
            other => panic!("expected Fts, got {other:?}"),
        }
    }

    #[test]
    fn to_sql_binds_match_expr_as_parameter_only() {
        // Arrange: Go "ToSql generates rowid IN subquery with MATCH". The
        // sanitized user text must appear ONLY as a bound parameter value,
        // never in the SQL text (security rule of the module).
        let Some(SearchStrategy::Fts(fts)) = new_fts_search("media_file", "beatles") else {
            panic!("expected Fts strategy");
        };
        // Act:
        let (sql, args) = fts.to_sql();
        // Assert:
        assert!(sql.contains("media_file.rowid IN"));
        assert!(sql.contains("media_file_fts"));
        assert!(sql.contains("MATCH ?"));
        assert!(
            !sql.contains("beatles"),
            "user text must not be interpolated"
        );
        assert_eq!(args, vec![Value::Text(fts.match_expr.clone())]);
    }

    #[test]
    fn rowid_select_binds_match_expr_at_join_position() {
        // Arrange: phase-1 rowidCore (sql_search_fts.go:233-240): rank
        // ordering first, qualified order-bys after, complex ones dropped.
        let Some(SearchStrategy::Fts(fts)) = new_fts_search("media_file", "beatles") else {
            panic!("expected Fts strategy");
        };
        let order_by = vec![
            "title asc".to_string(),
            "media_file.album desc".to_string(),
            "sum(x) desc".to_string(),
        ];
        // Act:
        let (sql, args) = fts.rowid_select(&order_by).to_sql().unwrap();
        // Assert:
        assert_eq!(
            sql,
            "SELECT media_file.rowid FROM media_file \
             INNER JOIN media_file_fts ON media_file_fts.rowid = media_file.rowid \
             AND media_file_fts MATCH ? \
             ORDER BY bm25(media_file_fts, 10.0, 5.0, 3.0, 3.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, \
             1.0), media_file.title asc, media_file.album desc"
        );
        assert!(
            !sql.contains("beatles"),
            "user text must not be interpolated"
        );
        assert_eq!(args, vec![Value::Text(fts.match_expr.clone())]);
    }

    #[test]
    fn count_path_applies_fts_filter_as_where_raw() {
        // Arrange: the REST filter/count path applies the FtsSearch fragment
        // through the builder for both select and count queries.
        let Some(SearchStrategy::Fts(fts)) = new_fts_search("album", "Sgt Peppers") else {
            panic!("expected Fts strategy");
        };
        let (filter_sql, filter_args) = fts.to_sql();
        // Act:
        let select = Repository::new("album")
            .where_raw(filter_sql.clone(), filter_args.clone())
            .limit(10);
        let count = Repository::new("album")
            .where_raw(filter_sql, filter_args)
            .count("album");
        let (select_sql, select_args) = select.to_sql().unwrap();
        let (count_sql, count_args) = count.to_sql().unwrap();
        // Assert:
        assert!(select_sql.starts_with("SELECT * FROM album WHERE album.rowid IN"));
        assert!(count_sql.contains("count(distinct album.id) as count"));
        assert!(select_sql.ends_with("LIMIT ?"));
        assert!(!select_sql.contains("Peppers"));
        assert!(!count_sql.contains("Peppers"));
        assert_eq!(select_args.len(), 2);
        assert_eq!(count_args, vec![Value::Text(fts.match_expr.clone())]);
    }
}
