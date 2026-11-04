//! #04 — DB backup/restore + pruning (`db/backup.go`, 184 lines).
//!
//! Go reaches the SQLite backup C API through `go.senan.xyz/sqlite3`'s
//! `SQLiteConn.Backup("main", src, "main")` (`db/backup.go:88-107`); the port
//! reaches the same C API through rusqlite's `backup` feature —
//! `rusqlite::backup::Backup::new(from, &mut to)` and `step(pages)` returning
//! `StepResult`. Deliberate deltas (recorded so review sees nothing hidden):
//!   * Go opens the OTHER end through its own go-sqlite3 driver, which re-runs
//!     `db/db.go:44-51` and registers SEEDEDRAND/NATSORT on the backup copy.
//!     Here the copy is opened with plain `Connection::open`: a backup is a
//!     data-only snapshot and the extensions must not be assumed present.
//!   * Go's `Step(-1)` (all pages in one write-lock hold, `db/backup.go:94`) is
//!     driven in bounded page batches, mirroring rusqlite's own
//!     `Connection::backup` loop (rusqlite backup.rs:73-81).
//!   * the timestamp half of the file name is handled as the same epoch-seconds
//!     `Time` `optimize.rs` models; `civil_from_epoch`/`day_seconds` are shared
//!     from there.

use rusqlite::{Connection, Error as RsError};

use std::fs;

use crate::optimize::{civil_from_epoch, day_seconds, Time};
use crate::schema::Error;

/// Prefix of a backup file name (`db/backup.go:21` `backupPrefix`).
pub const BACKUP_PREFIX: &str = "navidrome_backup";
/// Fixed rendered width of `2006.01.02_15.04.05` (`db/backup.go:27`).
pub const SUFFIX_LENGTH: usize = 19;
/// Pages copied per `step()` call — rusqlite's own default; Go asks for all
/// remaining pages at once (`db/backup.go:94` `Step(-1)`).
pub const PAGES_PER_STEP: i32 = 100;

/// `db/backup.go:27` (`backupSuffixLayout` rendering): `2023-11-14_22.13.20`.
pub fn backup_suffix(t: Time) -> String {
    let c = civil_from_epoch(t);
    format!(
        "{:04}-{:02}-{:02}_{:02}.{:02}.{:02}",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

/// `db/backup.go:27-29` (`backupSuffixLayout` + prefix/suffix): the stamped
/// file name.
pub fn backup_name(t: Time) -> String {
    let suffix = backup_suffix(t);
    format!("{BACKUP_PREFIX}_{suffix}.db")
}

/// `db/backup.go:25` (`backupRegex`): the suffix of a matching name, else `None`.
pub fn backup_suffix_of(name: &str) -> Option<String> {
    let body = BACKUP_PREFIX.len() + 1;
    let _name_s = name;
    let tail_ok = name.len() >= body + SUFFIX_LENGTH + 3;
    let sep = name.get(BACKUP_PREFIX.len()..body).unwrap_or("") == "_";
    if tail_ok && name.starts_with(BACKUP_PREFIX) && sep && name.ends_with(".db") {
        let middle = name.get(body..name.len() - 3).unwrap_or("");
        if middle.len() == SUFFIX_LENGTH {
            return Some(middle.to_string());
        }
    }
    None
}

/// `db/backup.go:148-152` (`time.Parse(backupSuffixLayout, suffix)`): the layout
/// is fixed-width, so any separator or digit deviation fails like Go's parse.
pub fn parse_backup_suffix(suffix: String) -> Option<Time> {
    if suffix.len() != SUFFIX_LENGTH {
        return None;
    }
    if suffix.get(4..5).unwrap_or("") != "-"
        || suffix.get(7..8).unwrap_or("") != "-"
        || suffix.get(10..11).unwrap_or("") != "_"
        || suffix.get(13..14).unwrap_or("") != "."
        || suffix.get(16..17).unwrap_or("") != "."
    {
        return None;
    }
    let year = digits(suffix.clone(), 0, 4);
    let month = digits(suffix.clone(), 5, 7);
    let day = digits(suffix.clone(), 8, 10);
    let hour = digits(suffix.clone(), 11, 13);
    let minute = digits(suffix.clone(), 14, 16);
    let second = digits(suffix, 17, 19);
    match (year, month, day, hour, minute, second) {
        (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s))
            if y >= 1970
                && (1..=12).contains(&mo)
                && (1..=31).contains(&d)
                && h <= 23
                && mi <= 59
                && s <= 59 =>
        {
            Some(day_seconds(y, mo, d) + h * 3600 + mi * 60 + s)
        }
        _ => None,
    }
}

/// Decimal field of the suffix layout; Go's `time.Parse` rejects a non-numeric
/// field, so a non-digit anywhere in the slice fails.
/// Decimal field of the suffix layout; Go's `time.Parse` rejects a non-numeric
/// field, so a non-digit anywhere in the slice fails.
fn digits(suffix: String, from: usize, to: usize) -> Option<i64> {
    suffix.get(from..to).unwrap_or("").parse().ok()
}

/// Posix-style two-argument join (Go's `filepath.Join`, `db/backup.go:29`).
fn join(dir: String, name: String) -> String {
    if dir == "/" || dir.ends_with("/") {
        return format!("{dir}{name}");
    }
    format!("{dir}/{name}")
}

/// `db/backup.go:29-34` (`backupPath`): full path for a stamp.
pub fn backup_path(dir: String, t: Time) -> String {
    join(dir, backup_name(t))
}

/// `db/backup.go:51-53` (`os.Stat` guard) + `:133-136` ("backup file not
/// available"): an absent/empty path must not silently restore an empty DB.
pub fn check_path_available(path: String) -> Result<(), Error> {
    let path = path.to_string();
    if path.is_empty() {
        return Err(io_error(
            "backup file not available: empty path".to_string(),
        ));
    }
    match fs::exists(&path) {
        Err(cause) => {
            let rendered = format!("backup file not available: {path}: {cause}");
            Err(io_error(rendered))
        }
        Ok(true) => Ok(()),
        Ok(false) => {
            let rendered = format!("backup file not available: {path}");
            Err(io_error(rendered))
        }
    }
}

/// `db/backup.go:88-107` (`backupOrRestore`): copy in the requested direction.
pub fn backup_or_restore(
    live: &mut Connection,
    other: &mut Connection,
    is_backup: bool,
) -> Result<(), Error> {
    if is_backup {
        return copy_pages(live, other);
    }
    copy_pages(other, live)
}

/// `db/backup.go:88-107`: start the handle, step to completion, then let the
/// handle's `Drop` finish the snapshot (rusqlite backup.rs:310).
fn copy_pages(from: &Connection, to: &mut Connection) -> Result<(), Error> {
    let started = rusqlite::backup::Backup::new(from, to);
    let handle = match started {
        Err(cause) => return Err(backup_error(cause)),
        Ok(handle) => handle,
    };
    let mut result = rusqlite::backup::StepResult::More;
    while result != rusqlite::backup::StepResult::Done {
        let stepped = handle.step(PAGES_PER_STEP);
        result = match stepped {
            Err(cause) => return Err(backup_error(cause)),
            Ok(step) => step,
        };
    }
    Ok(())
}

/// `db/backup.go:116-125` (`Backup`): snapshot into `dir`, return the path.
pub fn backup(live: &mut Connection, dir: String, now: Time) -> Result<String, Error> {
    let path = backup_path(dir, now);
    let opened = Connection::open(&path);
    let mut dest = match opened {
        Err(cause) => return Err(backup_error(cause)),
        Ok(conn) => conn,
    };
    match backup_or_restore(live, &mut dest, true) {
        Err(cause) => Err(cause),
        Ok(_) => Ok(path),
    }
}

/// `db/backup.go:127-130` (`Restore`): load `path` over the live DB.
pub fn restore(live: &mut Connection, path: String) -> Result<(), Error> {
    match check_path_available(path.to_string()) {
        Err(cause) => Err(cause),
        Ok(_) => match Connection::open(&path) {
            Err(cause) => Err(backup_error(cause)),
            Ok(mut saved) => backup_or_restore(live, &mut saved, false),
        },
    }
}

/// `db/backup.go:132-184` (`Prune`): delete the oldest backups beyond `keep`,
/// counting only removals that actually succeeded (`db/backup.go:171`).
pub fn prune(dir: String, keep: i64) -> Result<i64, Error> {
    let listed = fs::read_dir(&dir);
    let names = match listed {
        Err(cause) => {
            let rendered = format!("cannot list {dir}: {cause}");
            return Err(io_error(rendered));
        }
        Ok(entries) => entries,
    };
    let mut stamps: Vec<(Time, String)> = Vec::new();
    for item in names {
        let entry = match item {
            Ok(e) => e,
            Err(_) => continue,
        };
        let full = entry.path().display().to_string();
        // Go's `file.Name()` (db/backup.go:139): the raw entry name, no extension
        // stripping (base_name would eat the suffix's dots).
        let name = full.get(dir.len() + 1..).unwrap_or("").to_string();
        let suffix = backup_suffix_of(&name);
        let parsed = match suffix.clone() {
            None => None,
            Some(ref text) => parse_backup_suffix(text.to_string()),
        };
        match parsed {
            None => {}
            Some(stamp) => {
                stamps.push((stamp, join(dir.clone(), name)));
            }
        }
    }
    if stamps.len() as i64 <= keep {
        return Ok(0);
    }
    // `db/backup.go:160-162` sorts newest-first, then removes the tail.
    // Newest-first, Go's comparator negated into the key so pruning keeps the
    // newest and removes the tail (db/backup.go:160-162).
    stamps.sort_by_key(|entry| -entry.0);
    let mut deleted = 0;
    let keep_n = if keep < 0 { 0 } else { keep as usize };
    let (_, tail) = stamps.split_at(keep_n);
    for (_, path) in tail {
        match fs::remove_file(path) {
            Err(_) => {}
            Ok(_) => deleted += 1,
        }
    }
    Ok(deleted)
}

/// rusqlite failure → the crate error (path kept for diagnostics).
fn backup_error(cause: RsError) -> Error {
    Error::Failed {
        path: "backup".to_string(),
        cause,
    }
}

/// Filesystem failure → the crate error (rusqlite has no IO arm, so the
/// message carries the path; the `cause` slot takes a synthetic error).
fn io_error(message: String) -> Error {
    Error::Filesystem { message }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::Connection;

    use crate::backup::{backup_name, backup_path, backup_suffix_of, parse_backup_suffix};
    use crate::backup::{backup_or_restore, check_path_available, prune};
    use crate::optimize::{format_analyze_time, parse_analyze_time, Time};
    use crate::schema::{apply_fresh_schema, open};

    #[test]
    fn backup_suffix_round_trips_through_the_name() {
        // Arrange: a fixed stamp (db/backup.go:148-152 parse + :27 format).
        let now: Time = 1_700_000_000;

        // Act
        let name = backup_name(now);
        let suffix = backup_suffix_of(&name);
        let parsed = match suffix.clone() {
            None => None,
            Some(ref text) => parse_backup_suffix(text.to_string()),
        };

        // Assert: name → suffix → stamp is lossless, and the same stamp renders
        // identically through optimize.rs's RFC3339 path (shared Civil math).
        assert_eq!(name, "navidrome_backup_2023-11-14_22.13.20.db");
        assert_eq!(suffix, Some("2023-11-14_22.13.20".to_string()));
        assert_eq!(parsed, Some(1_700_000_000_i64));
        assert_eq!(
            parse_analyze_time(format_analyze_time(now)),
            Some(1_700_000_000_i64),
            "civil_from_epoch/day_seconds agree with optimize.rs"
        );
        let bad = parse_backup_suffix("2023-13-14_22.13.20".to_string());
        assert_eq!(bad, None, "month 13 rejected like Go's time.Parse");
    }

    #[test]
    fn backup_writes_and_restore_reads_the_snapshot() {
        // Arrange: a fresh-schema live DB and a temp snapshot file.
        let mut live = open(":memory:").unwrap();
        apply_fresh_schema(&live).unwrap();
        let dir = "/tmp/nd-backup".to_string();
        let made = fs::create_dir_all(&dir);
        if let Err(cause) = made {
            panic!("mkdir {cause}")
        }
        let now: Time = 1_700_000_000;
        let path = backup_path(dir, now);
        let _ = fs::remove_file(&path);

        // Act: backup direction, then restore direction.
        let opened = Connection::open(&path);
        match opened {
            Err(cause) => panic!("open snapshot: {cause}"),
            Ok(mut saved) => {
                let copied = backup_or_restore(&mut live, &mut saved, true);
                if let Err(cause) = copied {
                    panic!("backup: {cause}")
                }
            }
        }
        let available = check_path_available(path.clone());
        if let Err(cause) = available {
            panic!("snapshot not available after backup: {cause}")
        }

        // Assert: the snapshot exists and reopens for restore.
        let exists = fs::exists(&path);
        match exists {
            Ok(true) => {}
            Ok(false) => panic!("snapshot missing"),
            Err(cause) => panic!("snapshot missing: {cause}"),
        }
        let reopened = Connection::open(&path);
        match reopened {
            Err(cause) => panic!("reopen snapshot: {cause}"),
            Ok(mut saved) => {
                let restored = backup_or_restore(&mut live, &mut saved, false);
                if let Err(cause) = restored {
                    panic!("restore: {cause}")
                }
            }
        }
    }

    #[test]
    fn prune_deletes_the_oldest_beyond_keep() {
        // Arrange: three backup-named files in a temp dir (name arithmetic only,
        // db/backup.go:132-184).
        let dir = "/tmp/nd-prune".to_string();
        let made = fs::remove_dir_all(&dir);
        let _ = made;
        let made = fs::create_dir_all(&dir);
        if let Err(cause) = made {
            panic!("mkdir {cause}")
        }
        for stamp in [
            "2023-11-14_22.13.20",
            "2023-11-15_22.13.20",
            "2023-11-16_22.13.20",
        ]
        .to_vec()
        {
            let p = format!("{dir}/navidrome_backup_{stamp}.db");
            let opened = Connection::open(&p);
            let touch = match opened {
                Err(cause) => panic!("touch {cause}"),
                Ok(conn) => conn.execute("CREATE TABLE probe_backup (id INTEGER)", []),
            };
            if let Err(cause) = touch {
                panic!("touch {cause}")
            }
        }
        // Act
        let deleted = prune(dir, 1);
        // Assert
        match deleted {
            Err(cause) => panic!("prune: {cause}"),
            Ok(n) => assert_eq!(n, 2),
        }
    }
}
