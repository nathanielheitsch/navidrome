//! Folder fingerprint and the immutable snapshot the walker emits.
//!
//! Sources, mirrored:
//! - `scanner/folder_entry.go:106-151` (`hash()`) — MD5 over a fixed preimage, rendered
//!   as lowercase hex (`hex.EncodeToString`).
//! - `scanner/walk_dir_tree.go:108-181` (`loadDir`) — how `modTime`, `numSubFolders` and
//!   `imagesUpdatedAt` accumulate while entries are read in sorted order.
//! - `utils/time.go:11-19` (`TimeNewest`).
//! - `time.Time.String()` (`$GOROOT/src/time/format.go:546-547` (Go 1.27.4), format
//!   `"2006-01-02 15:04:05.999999999 -0700 UTC"` plus `appendNano` trimming at `:510-533`).
//!
//! ## Three distinct time encodings
//!
//! The **hash** spelling is Go's `UTC().String()` — `2023-11-14 22:13:20.123456789 +0000
//! UTC`, fractional seconds trimmed of trailing zeros — which is *not* RFC3339 and is
//! *not* the DB datetime representation. [`go_time_string`] reproduces that spelling; the
//! mapping layer and DB own their own encodings.
//!
//! ## Hash identity, stated plainly
//!
//! `nd_id::new_hash1` is not this hash: it inserts a U+200B separator and base62-encodes
//! (`nd-id/src/lib.rs:120-135`). The folder hash is **raw MD5 rendered lowercase hex**
//! over the exact preimage below, so an old Go-computed hash compares equal.
//!
//! ## Honest snapshot semantics
//!
//! A snapshot is an observed listing, not an atomic filesystem transaction: the directory
//! stat and every entry stat are taken during enumeration and are **not** re-read while
//! hashing. Nothing here re-stats a file, so a concurrent rename cannot make one hash
//! line disagree with another.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Timelike, Utc};
use md5::{Digest, Md5};

use crate::contract::ScanWarning;
use crate::storage::{FileStat, LogicalPath};

/// Errors specific to building a fingerprint from observed facts.
///
/// `io`-free by construction, so it can be `Clone`+`Eq` and compared in tests.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FolderSnapshotError {
    /// This platform reported no mtime for a fact the preimage needs. Go's
    /// `FileInfo.ModTime()` cannot fail; here the absence is surfaced.
    #[error("{path}: mtime not reported by this platform/filesystem")]
    TimestampUnavailable {
        /// The entry or directory affected.
        path: LogicalPath,
    },
    /// The timestamp is outside the range Go's `time.Time` (and `chrono`) can render.
    #[error("{path}: timestamp out of representable range for the Go time spelling")]
    TimestampOutOfRange {
        /// The entry or directory affected.
        path: LogicalPath,
    },
    /// A size exceeds Go's `int64`, so `%d` could not have rendered it.
    #[error("{path}: size {size} exceeds the range Go's int64 can render")]
    SizeOutOfRange {
        /// The entry affected.
        path: LogicalPath,
        /// The raw size.
        size: u64,
    },
}

/// Render a timestamp the way `time.Time.UTC().String()` does.
///
/// Shape: `YYYY-MM-DD HH:MM:SS[.fraction] +0000 UTC`, where the fraction is the
/// nanoseconds padded to 9 digits with trailing zeros removed (and the dot removed when
/// nothing is left). Years below 1000 are zero-padded to four digits; a negative year
/// renders as `-YYYY`; years above 9999 render unpadded. Sub-second times before the Unix
/// epoch keep Go's floor-second convention (nanoseconds stay positive).
pub fn go_time_string(time: SystemTime) -> Result<String, GoTimeError> {
    let (secs, nanos) = split_seconds(time).ok_or(GoTimeError::OutOfRange)?;
    let datetime: DateTime<Utc> =
        DateTime::from_timestamp(secs, nanos).ok_or(GoTimeError::OutOfRange)?;
    let date = datetime.date_naive();
    let time_of_day = datetime.time();

    let year = date.year();
    let mut out = String::with_capacity(30 + 10);
    if year < 0 {
        out.push('-');
        push_padded(&mut out, i64::from(year).unsigned_abs(), 4);
    } else {
        // The non-negative branch: years render unpadded above 9999, zero-padded below 1000.
        push_padded(&mut out, year.max(0) as u64, 4);
    }
    out.push('-');
    push_padded(&mut out, u64::from(date.month()), 2);
    out.push('-');
    push_padded(&mut out, u64::from(date.day()), 2);
    out.push(' ');
    push_padded(&mut out, u64::from(time_of_day.hour()), 2);
    out.push(':');
    push_padded(&mut out, u64::from(time_of_day.minute()), 2);
    out.push(':');
    push_padded(&mut out, u64::from(time_of_day.second()), 2);

    let nanos = time_of_day.nanosecond() % 1_000_000_000;
    if nanos != 0 {
        out.push('.');
        push_padded(&mut out, u64::from(nanos), 9);
        while out.ends_with('0') {
            out.pop();
        }
        if out.ends_with('.') {
            out.pop();
        }
    }
    out.push_str(" +0000 UTC");
    Ok(out)
}

/// Why a timestamp cannot be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoTimeError {
    /// Outside the range `chrono`/Go's `time.Time` can represent.
    OutOfRange,
}

/// Split a `SystemTime` into `(floor_seconds, nanoseconds_in_0..1e9)`, which is Go's
/// internal convention: nanoseconds are always non-negative, so an instant 1.5 s before the
/// epoch is `(-2, 500000000)`. Computed without nanosecond scaling so no representable
/// `SystemTime` can overflow; `None` only for durations beyond `chrono`'s range.
fn split_seconds(time: SystemTime) -> Option<(i64, u32)> {
    if time >= UNIX_EPOCH {
        let since = time
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0));
        // `as` is safe here: `DateTime::from_timestamp` rejects anything out of range.
        return Some((since.as_secs() as i64, since.subsec_nanos()));
    }
    // Floor of a negative instant: -(s + 1) seconds plus (1e9 - n) nanoseconds when the
    // sub-second part is non-zero, exactly like Go's internal (sec, nsec) pair.
    let before = UNIX_EPOCH
        .duration_since(time)
        .unwrap_or(Duration::from_secs(u64::MAX));
    let secs = before.as_secs();
    let nanos = before.subsec_nanos();
    if secs > i64::MAX as u64 {
        return None;
    }
    if nanos == 0 {
        Some((-(secs as i64), 0))
    } else {
        Some((-(secs as i64) - 1, 1_000_000_000 - nanos))
    }
}

fn push_padded(out: &mut String, value: u64, width: usize) {
    let text = value.to_string();
    for _ in text.len()..width {
        out.push('0');
    }
    out.push_str(&text);
}

/// What a classified entry contributes to a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryCategory {
    /// `model.IsAudioFile` on the classification name.
    Audio,
    /// `model.IsImageFile`.
    Image,
    /// `model.IsValidPlaylist`.
    Playlist,
    /// Anything else: counted in the listing, never in the fingerprint.
    Other,
}

/// One classified file, with the facts the fingerprint consumes and the endpoint facts
/// reserved for later mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    /// The directory entry's own name (the map key Go hashes).
    pub name: String,
    /// Logical path of the entry, i.e. what a DB row would store.
    pub path: LogicalPath,
    /// Name used for **classification**: the link's own name, or the resolved endpoint's
    /// base name for a followed symlink (`resolveEntryName`, `scanner/walk_dir_tree.go:253-298`).
    pub classification_name: String,
    /// Go's `fs.DirEntry.Info()` facts: the entry's own `lstat`. **This** is what the hash
    /// uses, so a symlink's hash line carries the link's size and mtime.
    pub stat: FileStat,
    /// Endpoint facts (`fs.Stat`), present only for a followed symlink. Reserved for tag
    /// enrichment in SC-03, never used by the hash.
    pub endpoint: Option<FileStat>,
    /// The resolved endpoint lies outside the library root.
    pub outside_root: bool,
    /// How the name was classified.
    pub category: EntryCategory,
}

/// Coverage of an emitted snapshot. Failed enumerations and ignored folders emit no
/// snapshot; exclusions from a completed listing are explicitly entry-incomplete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderCoverage {
    /// Every representable entry was enumerated and processed under policy.
    Complete,
    /// Some entries lacked captured metadata or a representable UTF-8 name.
    /// The fingerprint covers only the entries that could be represented.
    EntriesIncomplete,
}

/// An immutable folder snapshot: the facts a later phase would persist, plus the
/// fingerprint over them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSnapshot {
    /// Library the folder belongs to.
    pub library_id: i64,
    /// Normalized library-relative logical directory (`path.Clean` equivalent).
    pub path: LogicalPath,
    /// The directory's own observed stat (`fs.Stat(dirPath)` / `lstat` under
    /// follow-disabled).
    pub dir_stat: FileStat,
    /// `folderEntry.modTime`: the directory mtime raised to the newest **file** mtime
    /// seen, in sorted entry order (`scanner/walk_dir_tree.go:157-163`).
    pub mod_time: SystemTime,
    /// `folderEntry.imagesUpdatedAt`: newest of the image mtimes and the running
    /// `modTime`, or `None` when the folder has no images (Go's zero `time.Time`).
    pub images_updated_at: Option<SystemTime>,
    /// `folderEntry.numSubFolders`: child directories actually descended into.
    pub num_sub_folders: usize,
    /// All classified file entries, sorted by name bytes.
    pub entries: Vec<FolderEntry>,
    /// Coverage of this folder's enumeration.
    pub coverage: FolderCoverage,
    /// Warnings attributable to this folder, in emission order.
    pub warnings: Vec<ScanWarning>,
}

impl FolderSnapshot {
    /// `folderEntry.hasNoFiles()` (`scanner/folder_entry.go:60-62`).
    #[must_use]
    pub fn has_no_files(&self) -> bool {
        !self
            .entries
            .iter()
            .any(|entry| entry.category != EntryCategory::Other)
    }

    /// `folderEntry.isEmpty()` (`scanner/folder_entry.go:64-66`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.has_no_files() && self.num_sub_folders == 0
    }

    /// Entries of one category, sorted by name bytes — the order Go hashes them in
    /// (`scanner/folder_entry.go:117-123` sorts each map's keys).
    #[must_use]
    pub fn category(&self, category: EntryCategory) -> Vec<&FolderEntry> {
        let mut picked: Vec<&FolderEntry> = self
            .entries
            .iter()
            .filter(move |entry| entry.category == category)
            .collect();
        picked.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        picked
    }

    /// `folderEntry.numAudioFiles` equivalent.
    #[must_use]
    pub fn num_audio_files(&self) -> usize {
        self.category(EntryCategory::Audio).len()
    }

    /// Image names, sorted — the state a later phase compares against the persisted list
    /// (`scanner/folder_entry.go:72-77`).
    #[must_use]
    pub fn image_names(&self) -> Vec<&str> {
        self.category(EntryCategory::Image)
            .into_iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    /// The exact fingerprint preimage bytes, so a golden records its full input.
    ///
    /// Layout (`scanner/folder_entry.go:106-151`, in order):
    /// `<modTime>UTC:<playlistCount>:<numSubFolders>:<imagesUpdatedAt>UTC` then for each
    /// category in audio/image/playlist order, its keys sorted with
    /// `<name>:<size>:<mtime>UTC` appended per entry.
    pub fn hash_preimage(&self) -> Result<Vec<u8>, FolderSnapshotError> {
        let mut out = Vec::with_capacity(128);
        out.extend(
            go_time_string(self.mod_time)
                .map_err(|_| FolderSnapshotError::TimestampOutOfRange {
                    path: self.path.clone(),
                })?
                .into_bytes(),
        );
        out.push(b':');
        out.extend(
            self.category(EntryCategory::Playlist)
                .len()
                .to_string()
                .into_bytes(),
        );
        out.push(b':');
        out.extend(self.num_sub_folders.to_string().into_bytes());
        out.push(b':');
        // Go's zero `time.Time` renders as "0001-01-01 00:00:00 +0000 UTC"; a folder with
        // no images hashes exactly that spelling, so the absence is explicit, not epoch.
        let images_text = match self.images_updated_at {
            None => String::from("0001-01-01 00:00:00 +0000 UTC"),
            Some(images) => {
                go_time_string(images).map_err(|_| FolderSnapshotError::TimestampOutOfRange {
                    path: self.path.clone(),
                })?
            }
        };
        out.extend(images_text.into_bytes());

        for category in [
            EntryCategory::Audio,
            EntryCategory::Image,
            EntryCategory::Playlist,
        ] {
            for entry in self.category(category) {
                out.extend(entry.name.as_bytes());
                // An unavailable captured mtime is not an Info() failure in Go. Refuse
                // a partial preimage with a typed error rather than inventing a time.
                let modified =
                    entry
                        .stat
                        .modified
                        .map_err(|_| FolderSnapshotError::TimestampUnavailable {
                            path: entry.path.clone(),
                        })?;
                let rendered = go_time_string(modified).map_err(|_| {
                    FolderSnapshotError::TimestampOutOfRange {
                        path: entry.path.clone(),
                    }
                })?;
                if entry.stat.size > i64::MAX as u64 {
                    return Err(FolderSnapshotError::SizeOutOfRange {
                        path: entry.path.clone(),
                        size: entry.stat.size,
                    });
                }
                out.push(b':');
                out.extend(entry.stat.size.to_string().into_bytes());
                out.push(b':');
                out.extend(rendered.into_bytes());
            }
        }
        Ok(out)
    }

    /// The folder fingerprint: MD5 of [`Self::hash_preimage`], lowercase hex — the exact
    /// value Go stores, so an existing Go-computed hash compares equal.
    pub fn hash(&self) -> Result<String, FolderSnapshotError> {
        let preimage = self.hash_preimage()?;
        let mut digest = Md5::new();
        digest.update(&preimage);
        Ok(hex_lowercase(&digest.finalize()))
    }
}

/// Lowercase hex, like Go's `hex.EncodeToString`.
fn hex_lowercase(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        out.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    out
}

/// `utils.TimeNewest` over the observed timestamps.
#[must_use]
pub fn newest(times: &[SystemTime]) -> Option<SystemTime> {
    times.iter().max().copied()
}
