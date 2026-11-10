//! #05 slice-1 schema-drift guard — the Rust fresh DDL against the fully-migrated
//! Go schema dump (`docs/rust-port/next-work.md:80`). Go truth: the migrations
//! applied by `db/db.go` (esp. `db/migrations/20260220173400_add_fts5_search.go:28-46`,
//! the FTS5 feeder columns the `db9fdc01` fix guarded) plus
//! `docs/rust-port/schema-contract.md:177` (per-connection extension contract).
//!
//! The Go artifact is READ at test time, never regenerated or fabricated here:
//! it comes from the out-of-repo dump harness (`/tmp/nd-go-dump/ndgodump`, rc=0).

use nd_dbschema::optimize::{optimize_if_needed, Time};
use nd_dbschema::schema::{apply_fresh_schema, open};

use std::fs;
use std::process::Command;

/// The committed Go baseline: produced out-of-repo by the `nd-go-dump` harness
/// against the migrated Go tree (regenerate via docs/rust-port/next-work.md if
/// it ever drifts) and vendored as a golden fixture so fresh checkouts / CI run
/// the test unchanged. Path is relative to the crate root (cargo test cwd).
const GO_DUMP_PATH: &str = "tests/fixtures/go_schema_dump.txt";
const MISSING_GO_DUMP: &str = "Go-migrated .schema dump missing — regenerate via the nd-go-dump harness (see docs/rust-port/next-work.md) and re-run";

/// Names the Go dump carries but the Rust fresh-DDL golden deliberately drops
/// (`nd-dbschema/tests/fresh_schema.rs:60-65`: goose_db_version is migration
/// bookkeeping, plugin is another issue's table half): exactly this pair.
/// Count non-overlapping occurrences.
fn count_of(text: String, needle: String) -> usize {
    let mut found: usize = 0;
    let mut from: usize = 0;
    while from + needle.len() <= text.len() {
        let tail = text.get(from..text.len()).unwrap_or("");
        if tail.starts_with(&needle.to_string()) {
            found += 1;
            from += needle.len();
        } else {
            from += 1;
        }
    }
    found
}

/// First index where `needle` starts (scanning with starts_with — `find` rejects
/// `String` patterns on this toolchain).
fn first_at(text: String, needle: String) -> Option<usize> {
    let mut at: usize = 0;
    while at + needle.len() <= text.len() {
        let tail = text.get(at..text.len()).unwrap_or("");
        if tail.starts_with(&needle.to_string()) {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// The identifier run starting at or after `from` (alnum + underscore).
fn ident_at(text: String, from: usize) -> String {
    let mut to: usize = from;
    let mut done = false;
    while !done && to < text.len() {
        let slice = text.get(to..to + 1).unwrap_or("");
        let byte: u8 = slice.bytes().next().unwrap_or(0);
        let ok = byte == b'_' || byte.is_ascii_digit() || byte.is_ascii_alphabetic();
        if ok {
            to += 1;
        } else {
            done = true;
        }
    }
    text.get(from..to).unwrap_or("").to_string()
}

/// Every table name declared after `CREATE TABLE ` or `CREATE VIRTUAL TABLE `,
/// sorted and deduplicated (the `IF NOT EXISTS` form is skipped over by scanning
/// the name run that starts on an identifier character).
fn table_names(text: String) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for decl in ["CREATE TABLE ", "CREATE VIRTUAL TABLE "]
        .iter()
        .map(|x| x.to_string())
    {
        let mut from: usize = 0;
        let mut done = false;
        while !done {
            let tail = text.get(from..text.len()).unwrap_or("").to_string();
            match first_at(tail, decl.to_string()) {
                None => {
                    done = true;
                }
                Some(at) => {
                    let start = from + at + decl.len();
                    let mut name_start = start;
                    if ident_at(text.clone(), name_start) == "IF" {
                        name_start += "IF NOT EXISTS ".len();
                    }
                    let quoted = text.get(name_start..name_start + 1).unwrap_or("");
                    if quoted == "\"" {
                        let restq = text
                            .get(name_start + 1..text.len())
                            .unwrap_or("")
                            .to_string();
                        match first_at(restq.clone(), "\"".to_string()) {
                            None => {}
                            Some(q) => {
                                names.push(restq.get(0..q).unwrap_or("").to_string());
                            }
                        }
                    } else {
                        let name = ident_at(text.clone(), name_start);
                        if !name.is_empty() {
                            names.push(name.to_string());
                        }
                    }
                    from = name_start;
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// The column list of an fts5 virtual table: the text between its
/// `USING fts5(` and the matching `)`, split on commas, keeping only the
/// leading-identifier entries (options like `content=''` are dropped), sorted.
fn fts_columns(text: String, table: String) -> Vec<String> {
    let head: String = format!("{table} USING fts5(");
    let mut args: String = String::new();
    let mut from: usize = 0;
    let mut done = false;
    while !done && from <= text.len() {
        match first_at(text.clone(), head.clone()) {
            None => {
                done = true;
            }
            Some(at) => {
                let start = from + at + head.len();
                let rest = text.get(start..text.len()).unwrap_or("");
                match rest.find(")") {
                    None => {}
                    Some(close) => {
                        args = rest.get(0..close).unwrap_or("").to_string();
                    }
                }
                from = start;
            }
        }
    }
    let mut cols: Vec<String> = Vec::new();
    for piece in args.split(",") {
        let trimmed = piece.trim().to_string();
        if trimmed.find("=").is_none() && !trimmed.is_empty() {
            cols.push(ident_at(trimmed, 0).to_string());
        }
    }
    cols.sort();
    cols.dedup();
    cols
}

/// One index row: `(name, is_unique)`, parsed from a `.schema`-style dump.
///
/// Statements span several lines in the Go dump (`CREATE INDEX name` / `    ON t`
/// / `    (col);`), so the text is split on `;` and whitespace-collapsed before
/// the prefix test. `IF NOT EXISTS` is stripped because SQLite stores index SQL
/// without it (`sqlite_schema.sql`), which keeps both sides comparable.
fn index_rows(text: String) -> Vec<(String, bool)> {
    let mut rows: Vec<(String, bool)> = Vec::new();
    for chunk in text.split(";") {
        let mut folded: String = String::new();
        for piece in chunk.split_whitespace() {
            if !folded.is_empty() {
                folded.push(' ');
            }
            folded.push_str(piece);
        }
        let upper: String = folded.to_uppercase();
        let unique = upper.starts_with("CREATE UNIQUE INDEX ");
        let prefix: &str = if unique {
            "CREATE UNIQUE INDEX "
        } else if upper.starts_with("CREATE INDEX ") {
            "CREATE INDEX "
        } else {
            continue;
        };
        let rest: String = folded.chars().skip(prefix.len()).collect();
        let rest_upper: String = rest.to_uppercase();
        let trimmed = if rest_upper.starts_with("IF NOT EXISTS ") {
            rest.chars()
                .skip("IF NOT EXISTS ".len())
                .collect::<String>()
        } else {
            rest
        };
        let mut end: usize = 0;
        while end < trimmed.len() {
            let byte: u8 = trimmed.as_bytes()[end];
            if byte == b'_' || byte.is_ascii_alphanumeric() {
                end += 1;
            } else {
                break;
            }
        }
        rows.push((trimmed.chars().take(end).collect(), unique));
    }
    rows.sort();
    rows.dedup();
    rows
}

/// Count of rows whose unique flag is `want`.
fn count_unique(rows: Vec<(String, bool)>, want: bool) -> usize {
    rows.iter().filter(|row| row.1 == want).count()
}

/// Uniqueness of `name` in `rows` (None => the index is absent).
fn uniqueness(rows: Vec<(String, bool)>, name: String) -> Option<bool> {
    rows.iter().find(|row| row.0 == name).map(|row| row.1)
}

/// Table -> declared column names, from a `.schema`-style dump.
///
/// Statements span lines and `;`-splitting is the only reliable boundary here,
/// so chunks are whitespace-collapsed first. Top-level commas are found by
/// paren-depth tracking (a `UNIQUE (a, b)` constraint must not read as two
/// columns); table-level constraints (`CONSTRAINT x UNIQUE (...)`, a bare
/// `PRIMARY KEY (...)`) are skipped, which is what makes the comparison about
/// columns rather than about constraint spelling.
fn column_sets(text: String) -> Vec<(String, Vec<String>)> {
    let mut sets: Vec<(String, Vec<String>)> = Vec::new();
    for chunk in text.split(";") {
        let mut folded: String = String::new();
        for piece in chunk.split_whitespace() {
            if !folded.is_empty() {
                folded.push(' ');
            }
            folded.push_str(piece);
        }
        let upper: String = folded.to_uppercase();
        // Longest prefix first: "CREATE TABLE " is a prefix of the IF NOT EXISTS form.
        let decl: &str = if upper.starts_with("CREATE TABLE IF NOT EXISTS ") {
            "CREATE TABLE IF NOT EXISTS "
        } else if upper.starts_with("CREATE TABLE ") {
            "CREATE TABLE "
        } else {
            continue;
        };
        let rest: String = folded.chars().skip(decl.len()).collect();
        let rest = rest.replace("\"", "");
        let open = match rest.find("(") {
            Some(at) => at,
            None => continue,
        };
        let name = rest
            .chars()
            .take(open)
            .collect::<String>()
            .trim()
            .to_string();
        // sqlite_* bookkeeping and the single-quoted FTS5 auxiliary tables
        // ('album_fts_data' and friends) are not part of the contract.
        if name.is_empty()
            || name.starts_with("sqlite_")
            || name.find("'").is_some()
            || name.ends_with("_fts")
        {
            continue;
        }
        let body = match rest.get(open + 1..rest.len()) {
            Some(slice) => slice.to_string(),
            None => continue,
        };
        let mut cols: Vec<String> = Vec::new();
        let mut depth: usize = 0;
        let mut current: String = String::new();
        for ch in body.chars() {
            if ch == '(' {
                depth += 1;
                current.push(ch);
            } else if ch == ')' {
                if depth == 0 {
                    // End of the column list: the LAST column has no trailing
                    // comma, so flush the pending text before leaving the loop.
                    let trimmed = current.trim().to_string();
                    current = String::new();
                    if !trimmed.is_empty() {
                        let head: String = trimmed
                            .chars()
                            .take_while(|c| *c == '_' || c.is_ascii_alphanumeric())
                            .collect();
                        if !head.is_empty() {
                            cols.push(head);
                        }
                    }
                    break;
                }
                depth -= 1;
                current.push(ch);
            } else if ch == ',' && depth == 0 {
                let trimmed = current.trim().to_string();
                current = String::new();
                if trimmed.is_empty() {
                    continue;
                }
                let t_upper: String = trimmed.to_uppercase();
                if t_upper.starts_with("CONSTRAINT ")
                    || t_upper.starts_with("PRIMARY KEY (")
                    || t_upper.starts_with("UNIQUE (")
                    || t_upper.starts_with("UNIQUE(")
                    || t_upper.starts_with("CHECK (")
                    || t_upper.starts_with("FOREIGN KEY (")
                {
                    continue;
                }
                let head: String = trimmed
                    .chars()
                    .take_while(|c| *c == '_' || c.is_ascii_alphanumeric())
                    .collect();
                if !head.is_empty() {
                    cols.push(head);
                }
            } else {
                current.push(ch);
            }
        }
        cols.sort();
        sets.push((name, cols));
    }
    sets.sort();
    sets
}

/// Columns the dump `a` declares for `table` that `b` does not.
fn missing_columns(
    a: Vec<(String, Vec<String>)>,
    b: Vec<(String, Vec<String>)>,
    table: String,
) -> Vec<String> {
    let a_cols = match a.iter().find(|entry| entry.0 == table) {
        Some(entry) => entry.1.clone(),
        None => return Vec::new(),
    };
    let b_cols = match b.iter().find(|entry| entry.0 == table) {
        Some(entry) => entry.1.clone(),
        None => return a_cols,
    };
    let mut miss: Vec<String> = Vec::new();
    for name in a_cols.iter() {
        if b_cols.iter().find(|b| *b == name).is_none() {
            miss.push(name.to_string());
        }
    }
    miss
}

#[cfg(test)]
mod harness {
    use crate::*;

    #[test]
    fn rust_fresh_schema_matches_migrated_go_schema() {
        // Arrange: a private FILE-backed db of the previous run (never :memory:,
        // the shared-cache DSN aliases instances).
        let path = "/tmp/nd-db-schema-diff.sqlite";
        let _ = fs::remove_file(path);

        // Act: open fresh DDL + ANALYZE so sqlite_stat1 exists (optimize.rs
        // ANALYZE "name" path, same verb Go's optimize.go runs).
        let opened = open(path).unwrap();
        let mut conn = opened;
        apply_fresh_schema(&conn).unwrap();
        let now: Time = 1_700_000_000;
        optimize_if_needed(&mut conn, now).unwrap();

        // Act: dump via the sqlite3 CLI, the argv form that works on macOS
        // (`sqlite3 <db> .schema`; the CLI rejects -c).
        let dumped = Command::new("/usr/bin/sqlite3")
            .args([path.to_string(), ".schema".to_string()])
            .output();
        let output = match dumped {
            Err(cause) => panic!("sqlite3 dump: {cause}"),
            Ok(out) => out,
        };
        if !output.status.success() {
            panic!("sqlite3 .schema exit failed");
        }
        let bytes = output.stdout;
        let rust: String = String::from_utf8_lossy(&bytes).to_string();

        // Act: read the Go artifact; absence fails with the regeneration pointer.
        let go: String = match fs::read_to_string(GO_DUMP_PATH) {
            Err(_) => panic!("{MISSING_GO_DUMP}"),
            Ok(text) => text,
        };

        // Assert: the Go dump really is the migrated Go shape — it carries the
        // migration-bookkeeping and plugin tables the Rust golden drops.
        assert_eq!(
            count_of(go.clone(), "CREATE TRIGGER ".to_string()),
            9,
            "go: 9 FTS5 triggers"
        );
        assert_eq!(
            count_of(go.clone(), "CREATE VIRTUAL TABLE ".to_string()),
            3,
            "go: 3 fts5 tables"
        );
        assert_eq!(
            count_of(go.clone(), "sqlite_stat1".to_string()),
            1,
            "go: sqlite_stat1 present"
        );
        assert!(
            first_at(go.clone(), "goose_db_version".to_string()).is_some(),
            "go: migration bookkeeping present"
        );
        assert!(
            first_at(go.clone(), "CREATE TABLE plugin ".to_string()).is_some(),
            "go: plugin table present"
        );
        assert!(
            go.len() > 30_000,
            "go dump is the full migrated schema, not a stub"
        );

        // Assert: Rust side mirrors everything except the two dropped names.
        assert_eq!(
            count_of(rust.clone(), "CREATE TRIGGER ".to_string()),
            9,
            "rust: 9 FTS5 triggers"
        );
        assert_eq!(
            count_of(rust.clone(), "CREATE VIRTUAL TABLE ".to_string()),
            3,
            "rust: 3 fts5 tables"
        );
        assert_eq!(
            count_of(rust.clone(), "sqlite_stat1".to_string()),
            1,
            "rust: ANALYZE wrote sqlite_stat1"
        );
        assert_eq!(
            count_of(rust.clone(), "goose_db_version".to_string()),
            0,
            "rust golden drops goose_db_version"
        );
        assert_eq!(
            count_of(rust.clone(), "CREATE TABLE plugin ".to_string()),
            0,
            "rust golden drops plugin"
        );
        let rust_names = table_names(rust.clone());
        let go_names = table_names(go.clone());
        // sqlite_sequence/stat1/stat4 are SQLite bookkeeping rows (stat4 is an
        // artifact of the newer bundled SQLite's ANALYZE); compare the declared
        // table names, which must differ by exactly the dropped pair.
        let mut go_declared: Vec<String> = Vec::new();
        for name in go_names.clone() {
            if !name.starts_with("sqlite_") {
                go_declared.push(name.to_string());
            }
        }
        let mut rust_declared: Vec<String> = Vec::new();
        for name in rust_names.clone() {
            if !name.starts_with("sqlite_") {
                rust_declared.push(name.to_string());
            }
        }
        assert_eq!(
            go_declared.len() - rust_declared.len(),
            2,
            "declared-name delta is the pair"
        );
        let mut extra: Vec<String> = Vec::new();
        for name in go_declared.clone() {
            if rust_names.iter().find(|r| **r == name.as_ref()).is_none() {
                extra.push(name.to_string());
            }
        }
        let want = ["goose_db_version".to_string(), "plugin".to_string()];
        assert_eq!(extra, want, "name diff is exactly the pair");
        let mut rust_extra: Vec<String> = Vec::new();
        for name in rust_declared.clone() {
            if go_names.iter().find(|g| **g == name.as_ref()).is_none() {
                rust_extra.push(name.to_string());
            }
        }
        assert_eq!(
            rust_extra.len(),
            0,
            "rust declares nothing the go dump lacks"
        );

        // Assert: each of the 9 trigger names occurs exactly once in both dumps.
        for trigger in [
            "media_file_fts_ai",
            "media_file_fts_ad",
            "media_file_fts_au",
            "album_fts_ai",
            "album_fts_ad",
            "album_fts_au",
            "artist_fts_ai",
            "artist_fts_ad",
            "artist_fts_au",
        ]
        .iter()
        .map(|x| x.to_string())
        {
            assert_eq!(
                count_of(rust.clone(), trigger.clone()),
                1,
                "rust trigger {trigger} is present once"
            );
            assert_eq!(
                count_of(go.clone(), trigger.clone()),
                1,
                "go trigger {trigger} is present once"
            );
        }

        // Assert: the FTS5 feeder columns the migrations added are DECLARED on
        // both dumps (db9fdc01 drift guard): search_participants on media_file/
        // album, search_normalized on media_file/album/artist.
        assert_eq!(
            count_of(rust.clone(), "search_participants TEXT NOT NUL".to_string()),
            count_of(go.clone(), "search_participants TEXT NOT NUL".to_string()),
            "feeder declarations agree (both 2: media_file, album)",
        );
        assert_eq!(
            count_of(rust.clone(), "search_normalized TEXT NOT NUL".to_string()),
            count_of(go.clone(), "search_normalized TEXT NOT NUL".to_string()),
            "feeder declarations agree (both 3: media_file, album, artist)",
        );

        // Assert: the secondary index SET is identical on both sides. This is
        // the gap A13 opened: schema_ddl used to emit no CREATE INDEX at all, so
        // unindexed lookups were full scans and the player repository's API-key
        // uniqueness (the constraint that settles Save's insert races,
        // persistence/player_repository.go:152-157) had no backing at all. The
        // guard above could not catch it -- it compared only triggers and fts5
        // tables, so indexes were the blind spot of the drift guard itself.
        let go_indexes = index_rows(go.clone());
        let rust_indexes = index_rows(rust.clone());
        assert_eq!(
            go_indexes.len(),
            80,
            "go dump carries the full migrated index set"
        );
        assert_eq!(
            rust_indexes, go_indexes,
            "rust fresh schema declares EXACTLY the go index set (names + uniqueness)"
        );
        assert_eq!(count_unique(go_indexes.clone(), true), 4, "go: 4 unique");
        assert_eq!(
            count_unique(rust_indexes.clone(), true),
            4,
            "rust: 4 unique"
        );
        // The load-bearing indexes named explicitly, so a silent rename or drop
        // of one of them fails here rather than in production.
        assert_eq!(
            uniqueness(rust_indexes.clone(), "player_api_key_hash".to_string()),
            Some(true),
            "player_api_key_hash is UNIQUE in rust (API-key race settlement)"
        );
        assert_eq!(
            uniqueness(go_indexes.clone(), "player_api_key_hash".to_string()),
            Some(true),
            "player_api_key_hash is UNIQUE in go"
        );
        assert_eq!(
            uniqueness(rust_indexes.clone(), "scrobble_buffer_id_ix".to_string()),
            Some(true),
            "scrobble_buffer_id_ix is UNIQUE in rust"
        );
        assert_eq!(
            uniqueness(rust_indexes.clone(), "player_match".to_string()),
            Some(false),
            "player_match exists in rust (FindMatch, persistence/player_repository.go:61-70)"
        );

        // Assert: every declared COLUMN of every shared table matches. The
        // index pass is what found this class of drift: `media_file_codec`
        // could not even be created because the fresh DDL had dropped the
        // `codec` column that 20260307175815_add_codec_and_update_transcodings.go
        // adds, and eight columns across album/artist/media_file/playlist/radio
        // were missing outright. A schema whose indexes happen to be named the
        // same is not equivalent if a column is absent: it fails at CREATE INDEX
        // time, or silently at query time.
        // Assert: every declared COLUMN of every shared table matches. The index
        // pass is what surfaced this class of drift: `media_file_codec` could not
        // even be created, because the fresh DDL had dropped the `codec` column
        // added by 20260307175815_add_codec_and_update_transcodings.go:17, and
        // eight columns across album/artist/media_file/playlist/radio were absent
        // outright. Matching index NAMES are not enough -- a missing column fails
        // at CREATE INDEX time or silently at query time.
        let go_cols = column_sets(go.clone());
        let rust_cols = column_sets(rust.clone());
        assert!(
            go_cols.len() > 25,
            "column parser saw the whole go schema, not a stub"
        );
        // Shared tables only: the go dump additionally carries the two names the
        // golden drops, already pinned exactly by the table-name assertions.
        let mut shared: usize = 0;
        for entry in go_cols.clone() {
            let table = entry.0.clone();
            if rust_cols.iter().find(|r| r.0 == table).is_none() {
                continue;
            }
            shared += 1;
            assert_eq!(
                missing_columns(go_cols.clone(), rust_cols.clone(), table.clone()),
                Vec::<String>::new(),
                "rust {table} declares every column the go schema declares"
            );
            assert_eq!(
                missing_columns(rust_cols.clone(), go_cols.clone(), table.clone()),
                Vec::<String>::new(),
                "rust {table} declares nothing the go schema lacks"
            );
        }
        assert!(
            shared >= 30,
            "column sets compared across the whole shared table set (got {shared})"
        );
        // The eight columns the fresh DDL was missing, named explicitly so this
        // cannot regress silently again. Each was added by a late-window Go
        // migration whose up-section never made it into the golden DDL.
        for pair in [
            ("album", "rg_album_gain"),
            ("album", "rg_album_peak"),
            ("artist", "uploaded_image"),
            ("media_file", "codec"),
            ("media_file", "probe_data"),
            ("playlist", "uploaded_image"),
            ("playlist", "external_image_url"),
            ("radio", "uploaded_image"),
        ]
        .iter()
        .map(|x| (x.0.to_string(), x.1.to_string()))
        {
            let (table, column) = pair;
            assert!(
                missing_columns(go_cols.clone(), rust_cols.clone(), table.clone())
                    .iter()
                    .find(|c| **c == column)
                    .is_none(),
                "{table}.{column} is declared in the rust fresh schema"
            );
        }

        // Assert: the fts5 indexed-column sets are IDENTICAL per table — the
        // drift the earlier db9fdc01 fix guarded (trigger bodies reference
        // columns that must exist).
        for table in ["media_file_fts", "album_fts", "artist_fts"]
            .iter()
            .map(|x| x.to_string())
        {
            assert_eq!(
                fts_columns(rust.clone(), table.clone()),
                fts_columns(go.clone(), table.to_string()),
                "fts5 column sets identical for {table}",
            );
        }
    }
}
