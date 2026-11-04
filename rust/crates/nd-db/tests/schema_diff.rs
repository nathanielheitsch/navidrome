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

/// The Go artifact (never fabricated; absence is a hard failure).
const GO_DUMP_PATH: &str = "/tmp/nd-go-schema.txt";
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
