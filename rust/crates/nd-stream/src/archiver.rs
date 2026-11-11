//! Archiver — port of `core/archiver.go` (issue #15's zip/download slice).
//!
//! The zip-layout contract is pinned byte-for-byte against Go's `archive/zip`
//! by the oracle in `tests/data/go_zip/` (harness:
//! `tests/testdata/go_zip/main.go`), and the name-layout rules come from
//! `core/archiver_test.go`. Everything written here goes through [`ZipWriter`],
//! a faithful port of the subset of Go's `archive/zip.Writer` the archiver
//! exercises: `Store` method, data descriptor, extended-timestamp extra field,
//! UTF-8 flag detection, the EOCD comment, and the Zip64 arms.
//!
//! ## Trust boundary
//!
//! Like [`crate::direct`], the raw-file arm takes an **already-authorized
//! service-layer path**. Go reaches it through `mf.AbsolutePath()`
//! (`core/archiver.go:265`, `model/mediafile.go:255-257`), which derives from an
//! authorized DB row; this module performs no authorization and no library
//! scoping. For shares the share record *is* the authorization: Go returns
//! `model.ErrNotAuthorized` when `!share.Downloadable` and elevates only the
//! cover read (`core/archiver.go:182-191`).
//!
//! ## Divergences from Go, frozen here
//!
//!   - **No ambient context.** Go's `context.Context` (logged user, share
//!     elevation) becomes explicit parameters: [`CoverArt::get`] takes
//!     `elevated` in place of `request.WithUser(ctx, model.User{IsAdmin: true})`.
//!   - **Timestamps are UTC.** Go's cover-art entry uses `time.Now()`, whose
//!     MS-DOS fields are the **local** wall clock (`archive/zip/struct.go:273`),
//!     while the extended-timestamp extra field carries Unix seconds. This port
//!     encodes UTC in both. Consequence: on a host west of UTC a Go-built
//!     archive can carry a `folder.jpg` whose MS-DOS date is a day earlier than
//!     the Rust build's, and vice versa east of UTC. The Unix-seconds extra
//!     field — what unzip prefers when an extended timestamp is present — is
//!     identical, and track entries are unaffected because their timestamps come
//!     from the DB row (`UpdatedAt` is UTC).
//!   - **`time.Time` is `Option<i64>` (epoch seconds).** `None` mirrors Go's
//!     zero `time.Time`, which `CreateHeader` treats as "no MS-DOS fields, no
//!     extended timestamp" (`archive/zip/writer.go:326-349`).
//!   - **Deflate is not ported.** The archiver only ever writes `Store`
//!     (`core/archiver.go:287-291,336-339,296-299`), so [`ZipWriter::create_header`]
//!     rejects any other method with Go's `zip.ErrAlgorithm` equivalent. Go's
//!     `Deflate` arm would need a flate encoder this crate does not depend on.
//!   - **Only the limiter arm aborts.** Go checks exactly
//!     `errors.Is(addErr, stream.ErrTooManyTranscodes)` (`core/archiver.go:84-92,
//!     186-192`): a missing file or a mid-copy read failure is logged and the
//!     loop **continues**, which can leave a truncated entry — and in the
//!     playlist case a track that failed to zip still lands in the M3U, because
//!     Go assigns `mf.Path = file` unconditionally (`core/archiver.go:217-218`).
//!     Ported as written, not hardened.
//!   - **Share cover pick is explicit.** Go chooses a random track
//!     (`model/share.go:51-53`, `random.Int64N`), which no golden can reproduce;
//!     [`Archiver::zip_share`] passes the pick to
//!     [`ArchiveShare::cover_art_id`] as an index instead.
//!   - **Track filters are a marker enum, not SQL.** Go builds `squirrel`
//!     filters (`core/archiver.go:51,57-60`); SQL shaping belongs to `nd-db`
//!     (#05). [`TrackFilter`] records the same two shapes so the wiring layer
//!     compiles them and tests can assert which one was requested.
//!   - **Cover art is read whole.** Go buffers the image too ("Buffered so a
//!     failed read leaves no empty entry", `core/archiver.go:311-320`), so
//!     `Vec<u8>` matches Go's observable behavior exactly.
//!   - **The archive comment comes from the writer, not the method.** Go builds
//!     the `zip.Writer` inside each `Zip*` call; here [`create_zip_writer`] is
//!     explicit so the comment is testable and the caller keeps ownership of the
//!     sink.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io;
use std::pin::Pin;

use thiserror::Error;

use crate::limiter::TooManyTranscodes;
use crate::types::Request;

/// Go's boxed-future idiom for object-safe async traits: async traits are not
/// in this crate's dependency set (no `async-trait`), so trait methods return
/// `Pin<Box<dyn Future>>` and callers `.await` them normally.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// `archiveCoverArtSize` (`core/archiver.go:29`).
pub const ARCHIVE_COVER_ART_SIZE: i32 = 500;

/// EOCD comment Go writes for `format == "raw"` or `""` (`core/archiver.go:107`).
pub const DEFAULT_ZIP_COMMENT: &str = "Downloaded from Navidrome";

/// Chunk size of the copy loop — Go's `io.Copy` internal buffer (`io.copyLoop`).
pub const ZIP_COPY_CHUNK_BYTES: usize = 32 * 1024;

// ---------------------------------------------------------------------------
// zip writer — port of archive/zip's writer (Store path)
// ---------------------------------------------------------------------------

/// `zip.Store` / `zip.Deflate` (`archive/zip/struct.go:31-34`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZipMethod {
    /// No compression — the only method the archiver writes.
    Store,
    /// Deflate. Go's writer supports it; this port does not (module docs).
    Deflate,
}

impl ZipMethod {
    fn as_u16(self) -> u16 {
        match self {
            ZipMethod::Store => 0,
            ZipMethod::Deflate => 8,
        }
    }
}

/// Errors mirroring the arms Go's `archive/zip` returns.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ZipError {
    /// `zip.ErrAlgorithm` (`writer.go:387`): Go's `CreateHeader` returns it for
    /// a method with no registered compressor. Message copied verbatim.
    #[error("zip: unsupported compression algorithm")]
    UnsupportedMethod,
    /// `zip: Writer.Comment too long` (`writer.go:75`).
    #[error("zip: Writer.Comment too long")]
    CommentTooLong,
    /// `zip: write to closed file` (`writer.go:631`).
    #[error("zip: write to closed file")]
    WriteToClosedFile,
    /// `zip: file closed twice` (`writer.go:643`).
    #[error("zip: file closed twice")]
    FileClosedTwice,
    /// `zip: write to directory` (`writer.go:666-673`).
    #[error("zip: write to directory")]
    WriteToDirectory,
    /// `zip: writer closed twice` (`writer.go:92`).
    #[error("zip: writer closed twice")]
    ClosedTwice,
    /// An error from the underlying writer.
    #[error("zip: underlying writer: {0}")]
    Io(String),
}

impl From<io::Error> for ZipError {
    fn from(err: io::Error) -> Self {
        ZipError::Io(err.to_string())
    }
}

/// The `zip.FileHeader` fields the archiver sets (`Name`, `Modified`, `Method`)
/// plus `NonUTF8`, which Go's `CreateHeader` also honours (`writer.go:316-322`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipFileHeader {
    /// Entry name: relative path, forward slashes only.
    pub name: String,
    /// `Modified` — `None` is Go's zero `time.Time`.
    pub modified: Option<i64>,
    /// `Method`.
    pub method: ZipMethod,
    /// `NonUTF8`: clear the UTF-8 flag regardless of the name's content.
    pub non_utf8: bool,
}

impl ZipFileHeader {
    /// `zip.FileHeader{Name: name, Method: zip.Store}`, which is what every
    /// archiver entry uses.
    #[must_use]
    pub fn stored(name: impl Into<String>) -> Self {
        ZipFileHeader {
            name: name.into(),
            modified: None,
            method: ZipMethod::Store,
            non_utf8: false,
        }
    }
}

/// Buffered async sink — the port of the `bufio.Writer` inside Go's
/// `countWriter` (`archive/zip/writer.go:239-241,691-703`).
#[derive(Debug)]
struct Sink<W> {
    inner: W,
    buf: Vec<u8>,
    /// Bytes delivered to `inner`: exactly what Go's `countWriter.count` is,
    /// which drives entry offsets and the central-directory positions.
    count: u64,
}

impl<W: AsyncSink> Sink<W> {
    /// Go's `bufio.NewWriter` default buffer size. Flush boundaries do not
    /// change the bytes; the bound keeps memory flat like Go's.
    const BUF: usize = 4096;

    async fn write(&mut self, data: &[u8]) -> Result<(), ZipError> {
        if self.buf.len() + data.len() <= Self::BUF {
            self.buf.extend_from_slice(data);
            return Ok(());
        }
        self.flush().await?;
        if data.len() <= Self::BUF {
            self.buf.extend_from_slice(data);
            return Ok(());
        }
        self.inner.write_all(data).await?;
        self.count += data.len() as u64;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), ZipError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.inner.write_all(&self.buf).await?;
        self.count += self.buf.len() as u64;
        self.buf.clear();
        Ok(())
    }
}

/// Minimal async byte sink. Go's archiver writes into an `io.Writer` supplied
/// by the HTTP handler (`core/archiver.go:31-36`); the port uses async writes to
/// match the rest of the serving half (`crate::transcode_body::TranscodeBody::serve`).
pub trait AsyncSink {
    /// Write every byte of `data` to the sink.
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>>;
}

impl AsyncSink for Vec<u8> {
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.extend_from_slice(data);
            Ok(())
        })
    }
}

/// A finalized entry's central-directory record.
#[derive(Debug, Clone)]
struct EntryRecord {
    name: String,
    flags: u16,
    method: u16,
    modified: Option<i64>,
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    offset: u64,
}

/// A live zip entry — the port of Go's `fileWriter`
/// (`archive/zip/writer.go:566-663`). Writes are counted and CRC'd exactly as
/// Go does; sizes and CRC are finalized on close.
#[derive(Debug)]
pub struct ZipEntry<'a, W> {
    writer: &'a mut ZipWriter<W>,
    index: usize,
    directory: bool,
    closed: bool,
}

impl<W: AsyncSink> ZipEntry<'_, W> {
    /// `fileWriter.Write`: CRC the raw bytes, count them, hand them to the sink
    /// (`Store` means the stored size equals the raw size, like Go's
    /// `nopCompressor`).
    pub fn write<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<(), ZipError>> {
        Box::pin(async move {
            if self.closed {
                return Err(ZipError::WriteToClosedFile);
            }
            if self.directory {
                // Go's dirWriter accepts only an empty write (writer.go:665-672).
                if data.is_empty() {
                    return Ok(());
                }
                return Err(ZipError::WriteToDirectory);
            }
            self.writer.crc.update(data);
            self.writer.raw_sizes[self.index] += data.len() as u64;
            self.writer.write_bytes(data).await
        })
    }

    /// `fileWriter.close`: finalize sizes/CRC and emit the data descriptor.
    /// `ZipWriter::close` and the next `create_header` close it implicitly, as
    /// Go's `prepare` does (`writer.go:257-269`).
    pub fn close<'a>(&'a mut self) -> BoxFuture<'a, Result<(), ZipError>> {
        Box::pin(async move {
            if self.closed {
                return Err(ZipError::FileClosedTwice);
            }
            self.closed = true;
            if let Some(idx) = self.writer.open_entry {
                if idx == self.index {
                    self.writer.open_entry = None;
                }
            }
            self.writer.close_entry(self.index).await
        })
    }
}

/// Port of `zip.Writer` restricted to the `Store` path (module docs list the
/// arms that are not ported). Byte-for-byte compatible with Go for the headers
/// the archiver writes; pinned by `tests/archiver.rs` against the Go oracle.
#[derive(Debug)]
pub struct ZipWriter<W> {
    sink: Sink<W>,
    comment: String,
    entries: Vec<EntryRecord>,
    raw_sizes: Vec<u64>,
    crc: Crc32,
    /// Entry whose data is still open (Go's `w.last`).
    open_entry: Option<usize>,
    closed: bool,
}

impl<W: AsyncSink> ZipWriter<W> {
    /// `zip.NewWriter(w)` (`writer.go:239-241`).
    pub fn new(inner: W) -> Self {
        ZipWriter {
            sink: Sink {
                inner,
                buf: Vec::new(),
                count: 0,
            },
            comment: String::new(),
            entries: Vec::new(),
            raw_sizes: Vec::new(),
            crc: Crc32::new(),
            open_entry: None,
            closed: false,
        }
    }

    /// `Writer.SetComment` (`writer.go:72-79`): an over-limit comment is
    /// rejected and left unset, exactly as Go.
    pub fn set_comment(&mut self, comment: impl Into<String>) -> Result<(), ZipError> {
        let comment = comment.into();
        if comment.len() > u16::MAX as usize {
            return Err(ZipError::CommentTooLong);
        }
        self.comment = comment;
        Ok(())
    }

    /// Entries recorded so far (`len(w.dir)`).
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// The comment that will be written at `close`.
    #[must_use]
    pub fn comment(&self) -> &str {
        &self.comment
    }

    /// `Writer.CreateHeader` (`writer.go:292-411`).
    pub fn create_header<'a>(
        &'a mut self,
        header: &'a ZipFileHeader,
    ) -> BoxFuture<'a, Result<ZipEntry<'a, W>, ZipError>> {
        Box::pin(async move {
            // `prepare` (writer.go:257-269): close the previous entry first.
            if let Some(idx) = self.open_entry.take() {
                self.close_entry(idx).await?;
            }
            if self.closed {
                return Err(ZipError::ClosedTwice);
            }
            if header.method != ZipMethod::Store {
                return Err(ZipError::UnsupportedMethod);
            }

            // UTF-8 flag (writer.go:308-322). Go also inspects the per-entry
            // comment, which the archiver never sets.
            let mut flags: u16 = 0;
            let (valid, require) = detect_utf8(&header.name);
            if header.non_utf8 {
                flags &= !0x800;
            } else if require && valid {
                flags |= 0x800;
            }

            let directory = header.name.ends_with('/');
            if directory {
                flags &= !0x8;
            } else {
                flags |= 0x8; // data descriptor, always (writer.go:374)
            }

            let (mod_date, mod_time) = header.modified.map(ms_dos_time).unwrap_or((0, 0));
            let extra = extended_timestamp(header.modified);

            let offset = self.sink.count + self.sink.buf.len() as u64;
            let index = self.entries.len();
            self.raw_sizes.push(0);
            // Go builds a fresh `crc32.NewIEEE()` per fileWriter (`writer.go:386`),
            // so each entry CRCs only its own data.
            self.crc = Crc32::new();
            self.entries.push(EntryRecord {
                name: header.name.clone(),
                flags,
                method: header.method.as_u16(),
                modified: header.modified,
                crc: 0,
                compressed_size: 0,
                uncompressed_size: 0,
                offset,
            });

            // Local File Header. Go writes zero CRC/sizes because the
            // data-descriptor flag is always set (`writer.go:452-470`).
            let mut lfh: Vec<u8> = Vec::with_capacity(30 + header.name.len() + extra.len());
            lfh.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            lfh.extend_from_slice(&20u16.to_le_bytes()); // zipVersion20
            lfh.extend_from_slice(&flags.to_le_bytes());
            lfh.extend_from_slice(&header.method.as_u16().to_le_bytes());
            lfh.extend_from_slice(&mod_time.to_le_bytes());
            lfh.extend_from_slice(&mod_date.to_le_bytes());
            lfh.extend_from_slice(&0u32.to_le_bytes()); // crc32
            lfh.extend_from_slice(&0u32.to_le_bytes()); // compressed size
            lfh.extend_from_slice(&0u32.to_le_bytes()); // uncompressed size
            lfh.extend_from_slice(&(header.name.len() as u16).to_le_bytes());
            lfh.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            lfh.extend_from_slice(header.name.as_bytes());
            lfh.extend_from_slice(&extra);
            self.write_bytes(&lfh).await?;

            if directory {
                // Go finalizes a directory entry immediately: its sizes are
                // zero by definition (`writer.go:363-372`).
                self.close_entry(index).await?;
                self.open_entry = None;
            } else {
                self.open_entry = Some(index);
            }
            Ok(ZipEntry {
                writer: self,
                index,
                directory,
                // Go's `dirWriter` is not a `fileWriter`: it has no closed
                // state and `w.last` is nil for a directory, so `prepare`
                // never finalizes it (`writer.go:376,404`).
                closed: false,
            })
        })
    }

    /// `Writer.Close` (`writer.go:82-216`): finalize the open entry, write the
    /// central directory, then the end-of-central-directory record.
    pub fn close<'a>(&'a mut self) -> BoxFuture<'a, Result<(), ZipError>> {
        Box::pin(async move {
            if let Some(idx) = self.open_entry.take() {
                self.close_entry(idx).await?;
            }
            if self.closed {
                return Err(ZipError::ClosedTwice);
            }
            self.closed = true;

            let start = self.sink.count + self.sink.buf.len() as u64;
            let mut used_zip64 = false;
            let uint32max: u64 = u32::MAX as u64;

            // Clone each record out of `self.entries` so the write may borrow
            // `self` mutably while the record is still in hand.
            for entry in self.entries.clone() {
                let mut reader_version: u16 = 20;
                let extra = extended_timestamp(entry.modified);
                // Zip64 extra when any size or offset reaches 4 GiB - 1
                // (`writer.go:110-135`).
                if entry.uncompressed_size >= uint32max
                    || entry.compressed_size >= uint32max
                    || entry.offset >= uint32max
                {
                    used_zip64 = true;
                    reader_version = reader_version.max(45);
                    let mut body: Vec<u8> = Vec::new();
                    let mut size: u16 = 0;
                    if entry.uncompressed_size >= uint32max {
                        body.extend_from_slice(&entry.uncompressed_size.to_le_bytes());
                        size += 8;
                    }
                    if entry.compressed_size >= uint32max {
                        body.extend_from_slice(&entry.compressed_size.to_le_bytes());
                        size += 8;
                    }
                    if entry.offset >= uint32max {
                        body.extend_from_slice(&entry.offset.to_le_bytes());
                        size += 8;
                    }
                    let mut z64_extra: Vec<u8> = Vec::new();
                    z64_extra.extend_from_slice(&0x0001u16.to_le_bytes());
                    z64_extra.extend_from_slice(&size.to_le_bytes());
                    z64_extra.extend_from_slice(&body);
                    let combined: Vec<u8> = extra.into_iter().chain(z64_extra).collect();
                    self.write_central_directory_entry(&entry, reader_version, &combined)
                        .await?;
                    continue;
                }
                self.write_central_directory_entry(&entry, reader_version, &extra)
                    .await?;
            }

            let end = self.sink.count + self.sink.buf.len() as u64;
            let records = self.entries.len() as u64;
            let size = end - start;
            let offset = start;

            // Go emits Zip64 EOCD records whenever any entry needed a Zip64
            // extra field, even when the EOCD's own fields fit (`writer.go:183-206`).
            if used_zip64 || records >= u16::MAX as u64 || size >= uint32max || offset >= uint32max
            {
                let mut z64: Vec<u8> = Vec::with_capacity(76);
                z64.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
                z64.extend_from_slice(&44u64.to_le_bytes()); // 56 - 12
                z64.extend_from_slice(&45u16.to_le_bytes()); // version made by
                z64.extend_from_slice(&45u16.to_le_bytes()); // version needed
                z64.extend_from_slice(&0u32.to_le_bytes()); // this disk
                z64.extend_from_slice(&0u32.to_le_bytes()); // disk with CD start
                z64.extend_from_slice(&records.to_le_bytes());
                z64.extend_from_slice(&records.to_le_bytes());
                z64.extend_from_slice(&size.to_le_bytes());
                z64.extend_from_slice(&offset.to_le_bytes());
                z64.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
                z64.extend_from_slice(&0u32.to_le_bytes());
                z64.extend_from_slice(&end.to_le_bytes());
                z64.extend_from_slice(&1u32.to_le_bytes()); // total disks
                self.write_bytes(&z64).await?;
            }

            let mut eocd: Vec<u8> = Vec::with_capacity(22 + self.comment.len());
            eocd.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
            eocd.extend_from_slice(&0u16.to_le_bytes()); // this disk
            eocd.extend_from_slice(&0u16.to_le_bytes()); // disk with CD start
            eocd.extend_from_slice(&(records.min(u16::MAX as u64) as u16).to_le_bytes());
            eocd.extend_from_slice(&(records.min(u16::MAX as u64) as u16).to_le_bytes());
            eocd.extend_from_slice(&(size.min(uint32max) as u32).to_le_bytes());
            eocd.extend_from_slice(&(offset.min(uint32max) as u32).to_le_bytes());
            eocd.extend_from_slice(&(self.comment.len() as u16).to_le_bytes());
            eocd.extend_from_slice(self.comment.as_bytes());
            self.write_bytes(&eocd).await?;

            self.sink.flush().await
        })
    }

    /// One central-directory record (`writer.go:137-167`).
    fn write_central_directory_entry<'a>(
        &'a mut self,
        entry: &'a EntryRecord,
        reader_version: u16,
        extra: &'a [u8],
    ) -> BoxFuture<'a, Result<(), ZipError>> {
        Box::pin(async move {
            let (mod_date, mod_time) = entry.modified.map(ms_dos_time).unwrap_or((0, 0));
            let uint32max: u64 = u32::MAX as u64;
            let mut cd: Vec<u8> = Vec::with_capacity(46 + entry.name.len() + extra.len());
            cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            // CreatorVersion: Go sets `fh.CreatorVersion&0xff00 | zipVersion20`,
            // and the archiver never sets the creator byte, so it is 20.
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&reader_version.to_le_bytes());
            cd.extend_from_slice(&entry.flags.to_le_bytes());
            cd.extend_from_slice(&entry.method.to_le_bytes());
            cd.extend_from_slice(&mod_time.to_le_bytes());
            cd.extend_from_slice(&mod_date.to_le_bytes());
            cd.extend_from_slice(&entry.crc.to_le_bytes());
            cd.extend_from_slice(&(entry.compressed_size.min(uint32max) as u32).to_le_bytes());
            cd.extend_from_slice(&(entry.uncompressed_size.min(uint32max) as u32).to_le_bytes());
            cd.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment length
            cd.extend_from_slice(&0u16.to_le_bytes()); // disk number start
            cd.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            cd.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            cd.extend_from_slice(&(entry.offset.min(uint32max) as u32).to_le_bytes());
            cd.extend_from_slice(entry.name.as_bytes());
            cd.extend_from_slice(extra);
            self.write_bytes(&cd).await
        })
    }

    /// Write through the buffer, counting bytes like Go's `countWriter`.
    fn write_bytes<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, Result<(), ZipError>> {
        Box::pin(async move { self.sink.write(data).await })
    }

    /// Finalize one entry: set CRC and sizes, then write the data descriptor
    /// (`fileWriter.close`, `writer.go:641-663`).
    fn close_entry<'a>(&'a mut self, index: usize) -> BoxFuture<'a, Result<(), ZipError>> {
        Box::pin(async move {
            let raw = self.raw_sizes[index];
            let flags = self.entries[index].flags;
            let crc = self.crc.sum();
            // Go resets the hash per entry: the fileWriter owns its own
            // `crc32.NewIEEE()` (`writer.go:386`), so each entry CRCs only its
            // own data.
            self.entries[index].crc = crc;
            self.entries[index].compressed_size = raw; // Store
            self.entries[index].uncompressed_size = raw;
            if flags & 0x8 == 0 {
                return Ok(());
            }
            let uint32max: u64 = u32::MAX as u64;
            let mut dd: Vec<u8> = Vec::new();
            dd.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
            dd.extend_from_slice(&crc.to_le_bytes());
            if raw > uint32max {
                dd.extend_from_slice(&raw.to_le_bytes());
                dd.extend_from_slice(&raw.to_le_bytes());
            } else {
                dd.extend_from_slice(&(raw as u32).to_le_bytes());
                dd.extend_from_slice(&(raw as u32).to_le_bytes());
            }
            self.write_bytes(&dd).await
        })
    }

    /// Hand the sink back after [`ZipWriter::close`], so a caller that wrote
    /// into a `Vec<u8>` can read the archive.
    pub fn into_inner(self) -> W {
        self.sink.inner
    }
}

/// IEEE CRC-32 — Go's `crc32.NewIEEE()` / `crc32.ChecksumIEEE`.
///
/// One instance per entry, matching Go's per-`fileWriter` hash.
#[derive(Debug, Clone)]
pub struct Crc32 {
    value: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A fresh IEEE CRC-32 (`crc32.NewIEEE()`).
    #[must_use]
    pub fn new() -> Self {
        Crc32 { value: 0xFFFF_FFFF }
    }

    /// Feed bytes into the running CRC (`hash.Hash.Write`).
    pub fn update<'a>(&'a mut self, data: &'a [u8]) {
        for &b in data {
            let idx = ((self.value ^ u32::from(b)) & 0xff) as usize;
            self.value = (self.value >> 8) ^ CRC32_TABLE[idx];
        }
    }

    /// The finalized CRC (`hash.Hash32.Sum32`).
    #[must_use]
    pub fn sum(&self) -> u32 {
        self.value ^ 0xFFFF_FFFF
    }
}

/// `crc32.ChecksumIEEE(data)`.
#[must_use]
pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.sum()
}

/// `crc32.MakeTable(crc32.IEEE)` for the reflected polynomial 0xEDB88320.
const CRC32_TABLE: [u32; 256] = build_crc32_table();

const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// `archive/zip.detectUTF8` (`writer.go:246-265`). A Rust `&'a str` is always
/// valid UTF-8, so `valid` is always true (Go's `valid=false` arm needs invalid
/// bytes); `require` follows Go's rule, which also treats `~` (0x7e) and `\`
/// (0x5c) as requiring UTF-8 because CP-437 readers map them elsewhere.
#[must_use]
fn detect_utf8(s: &str) -> (bool, bool) {
    let mut require = false;
    for r in s.chars() {
        let cp = u32::from(r);
        if !(0x20..=0x7d).contains(&cp) || cp == 0x5c {
            require = true;
        }
    }
    (true, require)
}

/// The extended-timestamp extra field Go appends when `Modified` is set
/// (`archive/zip/writer.go:341-349`): id `0x5455`, size 5, flags 1 (ModTime),
/// then Unix seconds.
fn extended_timestamp(modified: Option<i64>) -> Vec<u8> {
    let Some(secs) = modified else {
        return Vec::new();
    };
    let mut extra: Vec<u8> = Vec::with_capacity(9);
    extra.extend_from_slice(&0x5455u16.to_le_bytes());
    extra.extend_from_slice(&5u16.to_le_bytes());
    extra.push(1); // flags: ModTime
    extra.extend_from_slice(&(secs as u32).to_le_bytes());
    extra
}

/// `archive/zip.timeToMsDosTime` (`struct.go:273-277`) over a UTC instant.
/// Out-of-range values are computed in `i64` and truncated, matching Go's
/// `uint16(...)` conversion. See the module docs for the UTC-vs-local
/// divergence on `time.Now()` cover entries.
#[must_use]
pub fn ms_dos_time(unix_secs: i64) -> (u16, u16) {
    let days = floor_div(unix_secs, 86_400);
    let rem = unix_secs - days * 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    // Go: `uint16(t.Day() + int(t.Month())<<5 + (t.Year()-1980)<<9)`; `<<` binds
    // tighter than `+`, so each term is shifted before the sum.
    let date = (i64::from(day) + (i64::from(month) << 5) + ((year - 1980) << 9)) as u16;
    let time = (second / 2 + (minute << 5) + (hour << 11)) as u16;
    (date, time)
}

/// Floor division — Go's time internals round toward negative infinity.
const fn floor_div(n: i64, d: i64) -> i64 {
    if n >= 0 {
        n / d
    } else {
        (n - d + 1) / d
    }
}

/// Days-from-civil (Howard Hinnant) — the date parts of `time.Unix(sec, 0).UTC()`.
#[must_use]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // 0..=399
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 0..=365
    let mp = (5 * doy + 2) / 153; // 0..=11
    let d = doy - (153 * mp + 2) / 5 + 1; // 1..=31
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // 1..=12
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// content sniffing — port of net/http.DetectContentType
// ---------------------------------------------------------------------------

/// Go's `SniffLen` (`net/http/internal/sniff.go:14`).
const SNIFF_LEN: usize = 512;

const fn is_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

const fn is_tt(b: u8) -> bool {
    matches!(b, b' ' | b'>')
}

/// `http.DetectContentType` (`net/http/sniff.go:15-24`), ported signature by
/// signature in Go's table order. Go's `coverArtExtension`
/// (`core/archiver.go:350-362`) maps only jpeg/png/webp/gif to an extension, so
/// every other result means "no cover art added to the zip".
#[must_use]
pub fn detect_content_type(data: &[u8]) -> &'static str {
    let data = if data.len() > SNIFF_LEN {
        &data[..SNIFF_LEN]
    } else {
        data
    };
    let mut first_non_ws = 0usize;
    while first_non_ws < data.len() && is_ws(data[first_non_ws]) {
        first_non_ws += 1;
    }

    for sig in [
        "<!DOCTYPE HTML",
        "<HTML",
        "<HEAD",
        "<SCRIPT",
        "<IFRAME",
        "<H1",
        "<DIV",
        "<FONT",
        "<TABLE",
        "<A",
        "<STYLE",
        "<TITLE",
        "<B",
        "<BODY",
        "<BR",
        "<P",
        "<!--",
    ] {
        if html_match(data, first_non_ws, sig) {
            return "text/html; charset=utf-8";
        }
    }
    if masked(data, first_non_ws, b"<?xml", b"\xFF\xFF\xFF\xFF\xFF", true) {
        return "text/xml; charset=utf-8";
    }
    if data.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    if data.starts_with(b"%!PS-Adobe-") {
        return "application/postscript";
    }
    if masked(
        data,
        first_non_ws,
        b"\xFE\xFF\x00\x00",
        b"\xFF\xFF\x00\x00",
        false,
    ) {
        return "text/plain; charset=utf-16be";
    }
    if masked(
        data,
        first_non_ws,
        b"\xFF\xFE\x00\x00",
        b"\xFF\xFF\x00\x00",
        false,
    ) {
        return "text/plain; charset=utf-16le";
    }
    if masked(
        data,
        first_non_ws,
        b"\xEF\xBB\xBF\x00",
        b"\xFF\xFF\xFF\x00",
        false,
    ) {
        return "text/plain; charset=utf-8";
    }
    // Image types, in Go's order.
    if data.starts_with(b"\x00\x00\x01\x00") || data.starts_with(b"\x00\x00\x02\x00") {
        return "image/x-icon";
    }
    if data.starts_with(b"BM") {
        return "image/bmp";
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if masked(
        data,
        first_non_ws,
        b"RIFF\x00\x00\x00\x00WEBPVP",
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
    ) {
        return "image/webp";
    }
    if data.starts_with(b"\x89PNG\x0D\x0A\x1A\x0A") {
        return "image/png";
    }
    if data.starts_with(b"\xFF\xD8\xFF") {
        return "image/jpeg";
    }
    // Audio and video, in the spec's mandated order.
    if masked(
        data,
        first_non_ws,
        b"FORM\x00\x00\x00\x00AIFF",
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        false,
    ) {
        return "audio/aiff";
    }
    if masked(data, first_non_ws, b"ID3", b"\xFF\xFF\xFF", false) {
        return "audio/mpeg";
    }
    if masked(
        data,
        first_non_ws,
        b"OggS\x00",
        b"\xFF\xFF\xFF\xFF\xFF",
        false,
    ) {
        return "application/ogg";
    }
    if masked(
        data,
        first_non_ws,
        b"MThd\x00\x00\x00\x06",
        b"\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
    ) {
        return "audio/midi";
    }
    if masked(
        data,
        first_non_ws,
        b"RIFF\x00\x00\x00\x00AVI ",
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        false,
    ) {
        return "video/avi";
    }
    if masked(
        data,
        first_non_ws,
        b"RIFF\x00\x00\x00\x00WAVE",
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        false,
    ) {
        return "audio/wave";
    }
    if mp4_match(data) {
        return "video/mp4";
    }
    if data.starts_with(b"\x1A\x45\xDF\xA3") {
        return "video/webm";
    }
    // Font types.
    if data[first_non_ws..].len() >= 36
        && data[first_non_ws + 34..first_non_ws + 36] == [0xFF, 0xFF]
        && data[first_non_ws..first_non_ws + 34]
            .iter()
            .all(|&b| b == 0)
    {
        return "application/vnd.ms-fontobject";
    }
    if data.starts_with(b"\x00\x01\x00\x00") {
        return "font/ttf";
    }
    if data.starts_with(b"OTTO") {
        return "font/otf";
    }
    if data.starts_with(b"ttcf") {
        return "font/collection";
    }
    if data.starts_with(b"wOFF") {
        return "font/woff";
    }
    if data.starts_with(b"wOF2") {
        return "font/woff2";
    }
    // Archive types.
    if data.starts_with(b"\x1F\x8B\x08") {
        return "application/x-gzip";
    }
    if data.starts_with(b"PK\x03\x04") {
        return "application/zip";
    }
    if data.starts_with(b"Rar!\x1A\x07\x00") || data.starts_with(b"Rar!\x1A\x07\x01\x00") {
        return "application/x-rar-compressed";
    }
    if data.starts_with(b"\x00\x61\x73\x6D") {
        return "application/wasm";
    }
    // textSig is last in Go's table.
    if data[first_non_ws..].iter().all(|&b| {
        !(b <= 0x08 || b == 0x0B || (0x0E..=0x1A).contains(&b) || (0x1C..=0x1F).contains(&b))
    }) {
        return "text/plain; charset=utf-8";
    }
    "application/octet-stream"
}

fn html_match<'a>(data: &'a [u8], first_non_ws: usize, sig: &'a str) -> bool {
    let d = &data[first_non_ws..];
    if d.len() < sig.len() + 1 {
        return false;
    }
    for (i, &b) in sig.as_bytes().iter().enumerate() {
        let mut db = d[i];
        if b.is_ascii_uppercase() {
            db &= 0xDF;
        }
        if b != db {
            return false;
        }
    }
    is_tt(d[sig.len()])
}

fn masked<'a>(
    data: &'a [u8],
    first_non_ws: usize,
    pat: &'a [u8],
    mask: &'a [u8],
    skip_ws: bool,
) -> bool {
    let d = if skip_ws { &data[first_non_ws..] } else { data };
    if pat.len() != mask.len() || d.len() < pat.len() {
        return false;
    }
    for i in 0..pat.len() {
        if d[i] & mask[i] != pat[i] {
            return false;
        }
    }
    true
}

/// `mp4Sig.match` (`net/http/internal/sniff.go:262-287`).
fn mp4_match(data: &[u8]) -> bool {
    if data.len() < 12 {
        return false;
    }
    let box_size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if data.len() < box_size || !box_size.is_multiple_of(4) {
        return false;
    }
    if &data[4..8] != b"ftyp" {
        return false;
    }
    let mut st = 8usize;
    while st < box_size {
        if st == 12 {
            // Skip the "major brand" version field.
            st += 4;
            continue;
        }
        if st + 3 <= data.len() && &data[st..st + 3] == b"mp4" {
            return true;
        }
        st += 4;
    }
    false
}

/// `coverArtExtension` (`core/archiver.go:350-362`): the extension for the
/// sniffed type; `""` means Go does not add the image to the zip.
#[must_use]
pub fn cover_art_extension(data: &[u8]) -> &'static str {
    match detect_content_type(data) {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// path helpers — port of path/filepath's unix semantics
// ---------------------------------------------------------------------------

/// `filepath.Join` on unix (`path/filepath/path.go:269-297`): drop empty
/// elements, join with `/`, then `Clean`.
#[must_use]
pub fn file_path_join(parts: &[&str]) -> String {
    // Go skips leading empty elements, then joins the rest (empties included)
    // and cleans the result.
    match parts.iter().position(|p| !p.is_empty()) {
        Some(i) => path_clean(&parts[i..].join("/")),
        None => String::new(),
    }
}

/// `filepath.Clean` on unix — a line-by-line port of `path.Clean`
/// (`path/path.go:72-139`), which `path/filepath` reuses on unix. The `dotdot`
/// marker is the index in the output where a `..` may not backtrack past, and
/// trailing separators are dropped (Go never appends one at the end).
#[must_use]
pub fn path_clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let rooted = path.starts_with('/');
    let bytes = path.as_bytes();
    let n = bytes.len();
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut dotdot = 0usize;
    if rooted {
        out.push(b'/');
        dotdot = 1;
    }
    let mut r = 0usize;
    while r < n {
        if bytes[r] == b'/' {
            // empty path element
            r += 1;
        } else if bytes[r] == b'.' && (r + 1 == n || bytes[r + 1] == b'/') {
            // . element
            r += 1;
        } else if bytes[r] == b'.'
            && r + 1 < n
            && bytes[r + 1] == b'.'
            && (r + 2 == n || bytes[r + 2] == b'/')
        {
            // .. element: remove to last '/'
            r += 2;
            if out.len() > dotdot {
                let mut w = out.len() - 1;
                while w > dotdot && out[w] != b'/' {
                    w -= 1;
                }
                out.truncate(w);
            } else if !rooted {
                if !out.is_empty() {
                    out.push(b'/');
                }
                out.extend_from_slice(b"..");
                dotdot = out.len();
            }
        } else {
            // real path element: add a separator if needed, then copy it
            if (rooted && out.len() != 1) || (!rooted && !out.is_empty()) {
                out.push(b'/');
            }
            while r < n && bytes[r] != b'/' {
                out.push(bytes[r]);
                r += 1;
            }
        }
    }
    if out.is_empty() {
        return ".".to_string();
    }
    String::from_utf8(out).unwrap_or_else(|_| String::new())
}

/// `filepath.Split` on unix (`path/filepath/path.go:299-313`): split at the
/// final separator, which stays in the first half.
#[must_use]
pub fn file_path_split(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(i) => (path[..=i].to_string(), path[i + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// `model.MediaFile.AbsolutePath` (`model/mediafile.go:255-257`).
#[must_use]
pub fn absolute_path(library_path: &str, path: &str) -> String {
    file_path_join(&[library_path, path])
}

/// `str.SanitizeFilename` (`utils/str/sanitize_strings.go`): reserved
/// filesystem characters become `_`. Reuses `nd-utils`' port.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    nd_utils::str::sanitize_filename(name)
}

// ---------------------------------------------------------------------------
// artwork ids
// ---------------------------------------------------------------------------

/// `model.ArtworkID` (`model/artwork_id.go:57-62`) narrowed to what the
/// archiver reads and renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtworkId {
    /// Kind prefix: `mf`, `ar`, `al`, `pl`, `dc`, `ra` (`model/artwork_id.go:26-31`).
    pub prefix: &'static str,
    /// Entity id; `""` means no artwork, which Go skips
    /// (`core/archiver.go:308-310`).
    pub id: String,
    /// Content-hash suffix; `""` = unknown/none.
    pub hash: String,
}

impl ArtworkId {
    /// `ArtworkID.String` (`model/artwork_id.go:64-72`).
    #[must_use]
    pub fn render(&self) -> String {
        if self.id.is_empty() {
            return String::new();
        }
        if self.hash.is_empty() {
            return format!("{}-{}", self.prefix, self.id);
        }
        format!("{}-{}_{}", self.prefix, self.id, self.hash)
    }

    /// `model.ArtworkID{}` — the zero value Go treats as "no cover".
    #[must_use]
    pub fn none() -> Self {
        ArtworkId {
            prefix: KIND_ALBUM,
            id: String::new(),
            hash: String::new(),
        }
    }
}

/// `model.KindAlbumArtwork.Prefix()` (`model/artwork_id.go:28`).
pub const KIND_ALBUM: &str = "al";
/// `model.KindArtistArtwork.Prefix()`.
pub const KIND_ARTIST: &str = "ar";
/// `model.KindPlaylistArtwork.Prefix()`.
pub const KIND_PLAYLIST: &str = "pl";
/// `model.KindDiscArtwork.Prefix()`.
pub const KIND_DISC: &str = "dc";
/// `model.KindMediaFileArtwork.Prefix()`.
pub const KIND_MEDIA_FILE: &str = "mf";

/// `artworkIDFromAlbum` (`model/artwork_id.go:150-152`).
#[must_use]
pub fn album_artwork_id(id: impl Into<String>, image_hash: impl Into<String>) -> ArtworkId {
    ArtworkId {
        prefix: KIND_ALBUM,
        id: id.into(),
        hash: image_hash.into(),
    }
}

/// `artworkIDFromArtist` (`model/artwork_id.go:162-164`). `Artist{ID: id}` —
/// the shape `ZipArtist` builds (`core/archiver.go:57`) — carries no hash.
#[must_use]
pub fn artist_artwork_id(id: impl Into<String>) -> ArtworkId {
    ArtworkId {
        prefix: KIND_ARTIST,
        id: id.into(),
        hash: String::new(),
    }
}

/// `artworkIDFromPlaylist` (`model/artwork_id.go:158-160`).
#[must_use]
pub fn playlist_artwork_id(id: impl Into<String>, image_hash: impl Into<String>) -> ArtworkId {
    ArtworkId {
        prefix: KIND_PLAYLIST,
        id: id.into(),
        hash: image_hash.into(),
    }
}

/// `DiscArtworkID` (`model/artwork_id.go:134-136`). Kept for the disc-artwork
/// fallback the archiver reaches through `mf.CoverArtID()` in the share arm.
#[must_use]
pub fn disc_artwork_id(album_id: &str, disc_number: i32) -> String {
    format!("{album_id}:{disc_number}")
}

// ---------------------------------------------------------------------------
// rows the archiver reads
// ---------------------------------------------------------------------------

/// The per-track fields the archiver reads. Go reads them from a hydrated
/// `model.MediaFile`; the decision-layer subset (`codec`, `bit_rate`,
/// `sample_rate`, `bit_depth`, `channels`, `size`, `probe_data`) travels along
/// so a [`RequestResolver`] adapter can build a [`crate::types::MediaFile`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArchiveMediaFile {
    /// `model.MediaFile.ID`.
    pub id: String,
    /// `model.MediaFile.LibraryPath` — the authorized root for [`absolute_path`].
    pub library_path: String,
    /// `model.MediaFile.Path` — relative to the library root.
    pub path: String,
    /// `model.MediaFile.Suffix`.
    pub suffix: String,
    /// `model.MediaFile.Artist`.
    pub artist: String,
    /// `model.MediaFile.Title`.
    pub title: String,
    /// `model.MediaFile.Album`.
    pub album: String,
    /// `model.MediaFile.AlbumID`.
    pub album_id: String,
    /// `model.MediaFile.AlbumImage.ImageHash` — the album's content hash. Go
    /// uses `AlbumCoverArtID`, not the track's own image
    /// (`core/archiver.go:95`, `model/mediafile.go:155-159`).
    pub album_image_hash: String,
    /// `model.MediaFile.HasCoverArt` — the track has embedded artwork. Only the
    /// share arm consumes it, through `CoverArtID` (`model/mediafile.go:137-144`).
    pub has_cover_art: bool,
    /// `model.MediaFile.ItemImage.ImageHash` — the track's own content hash, used
    /// by `CoverArtID` and by the disc fallback (`model/mediafile.go:148-153`).
    pub image_hash: String,
    /// `model.MediaFile.DiscNumber`.
    pub disc_number: i32,
    /// `model.MediaFile.Year`.
    pub year: i32,
    /// `model.MediaFile.ReleaseYear`.
    pub release_year: i32,
    /// `model.MediaFile.MbzAlbumType`.
    pub mbz_album_type: String,
    /// `model.MediaFile.CatalogNum`.
    pub catalog_num: String,
    /// `model.MediaFile.Tags` (`model/tag.go:79`). Only `albumversion` and
    /// `recordlabel` are read here.
    pub tags: BTreeMap<String, Vec<String>>,
    /// `model.MediaFile.UpdatedAt` — epoch seconds; `None` = Go's zero time.
    pub updated_at: Option<i64>,
    /// `model.MediaFile.Duration` — Go's `float32`, used by the M3U `#EXTINF`.
    pub duration: f32,
    /// `model.MediaFile.Codec`.
    pub codec: String,
    /// `model.MediaFile.BitRate` — kbps.
    pub bit_rate: i64,
    /// `model.MediaFile.SampleRate`.
    pub sample_rate: i64,
    /// `model.MediaFile.BitDepth` — Go's `*int`.
    pub bit_depth: Option<i64>,
    /// `model.MediaFile.Channels`.
    pub channels: i64,
    /// `model.MediaFile.Size`.
    pub size: i64,
    /// `model.MediaFile.ProbeData`.
    pub probe_data: String,
    /// `model.MediaFile.Missing`. The artist filter sets `missing = false`
    /// server-side; carried for parity with Go's row.
    pub missing: bool,
}

impl ArchiveMediaFile {
    /// `Tags.First(name)` (`model/tag.go:85-90`).
    #[must_use]
    pub fn tag_first(&self, name: &str) -> &str {
        self.tags
            .get(name)
            .and_then(|v| v.first())
            .map(String::as_str)
            .unwrap_or("")
    }

    /// `model.MediaFile.FullAlbumName` (`model/mediafile.go:116-121`).
    /// `append_album_version` replaces Go's
    /// `conf.Server.Subsonic.AppendAlbumVersion` (default true,
    /// `conf/configuration.go:1075`).
    #[must_use]
    pub fn full_album_name(&self, append_album_version: bool) -> String {
        let version = self.tag_first("albumversion");
        if append_album_version && !version.is_empty() {
            return append_suffix(&self.album, version);
        }
        self.album.clone()
    }

    /// `model.MediaFile.CoverArtID` (`model/mediafile.go:137-144`): the track's
    /// own artwork when it has one and the feature is on, otherwise the disc
    /// fallback, otherwise the album. Used by the share arm's random-track cover
    /// (`model/share.go:50`).
    #[must_use]
    pub fn cover_art_id(&self, options: &ArchiverOptions) -> ArtworkId {
        if self.has_cover_art && options.enable_media_file_cover_art {
            return ArtworkId {
                prefix: KIND_MEDIA_FILE,
                id: self.id.clone(),
                hash: self.image_hash.clone(),
            };
        }
        self.disc_cover_art_id()
    }

    /// `model.MediaFile.DiscCoverArtID` (`model/mediafile.go:148-153`).
    #[must_use]
    pub fn disc_cover_art_id(&self) -> ArtworkId {
        if self.disc_number > 0 {
            return ArtworkId {
                prefix: KIND_DISC,
                id: disc_artwork_id(&self.album_id, self.disc_number),
                hash: self.image_hash.clone(),
            };
        }
        self.album_cover_art_id()
    }

    /// `model.MediaFile.AlbumCoverArtID` (`model/mediafile.go:157-159`).
    #[must_use]
    pub fn album_cover_art_id(&self) -> ArtworkId {
        album_artwork_id(self.album_id.clone(), self.album_image_hash.clone())
    }

    /// `model.MediaFile.AbsolutePath` (`model/mediafile.go:255-257`).
    #[must_use]
    pub fn absolute_path(&self) -> String {
        absolute_path(&self.library_path, &self.path)
    }
}

/// `model.appendSuffix` (`model/mediafile.go:125-131`): a bracketed suffix is
/// appended as-is, otherwise wrapped in parentheses.
#[must_use]
pub fn append_suffix<'a>(base: &'a str, suffix: &'a str) -> String {
    let suffix = suffix.trim();
    let bytes = suffix.as_bytes();
    let bracketed = bytes.len() >= 2
        && matches!(
            (bytes[0], bytes[bytes.len() - 1]),
            (b'(', b')') | (b'[', b']') | (b'{', b'}') | (b'<', b'>')
        );
    if bracketed {
        return format!("{base} {suffix}");
    }
    format!("{base} ({suffix})")
}

/// Go's `QueryOptions.Filters` for the two queries the archiver builds. SQL
/// shaping is `nd-db` (#05); this is the request marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackFilter {
    /// `squirrel.Eq{"album_id": id}` (`core/archiver.go:51`).
    AlbumId(String),
    /// `squirrel.And{ParticipantIDFilter("media_file", id, RoleAlbumArtist),
    /// squirrel.Eq{"missing": false}}` (`core/archiver.go:57-60`). Go matches
    /// album-artist *participation*, not the deprecated `album_artist_id`
    /// column, so co-album-artists are included too.
    AlbumArtistParticipant {
        /// `model.RoleAlbumArtist` participant id.
        artist_id: String,
    },
}

/// `model.QueryOptions` narrowed to what `zipAlbums` sets
/// (`Filters` plus `Sort: "album"`, `core/archiver.go:66`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackQuery {
    /// Which rows to load.
    pub filter: TrackFilter,
    /// Go's `Sort` field, always `"album"` here.
    pub sort: String,
}

/// `model.Playlist` subset that `ZipPlaylist` consumes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArchivePlaylist {
    /// `model.Playlist.ID`.
    pub id: String,
    /// `model.Playlist.Name`.
    pub name: String,
    /// `model.Playlist.ImageHash`, feeding `CoverArtID`.
    pub image_hash: String,
    /// `Playlist.MediaFiles()` after `GetWithTracks(ctx, id, true, false)`.
    pub tracks: Vec<ArchiveMediaFile>,
}

impl ArchivePlaylist {
    /// `Playlist.CoverArtID` (`model/playlist.go:134-136`).
    #[must_use]
    pub fn cover_art_id(&self) -> ArtworkId {
        playlist_artwork_id(self.id.clone(), self.image_hash.clone())
    }
}

/// `model.Share` subset (`model/share.go:13-33`) as the archiver uses it. The
/// record is **already loaded**: `Share.Load` records a visit, so re-loading
/// here would double-count every download (`core/archiver.go:180-182`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArchiveShare {
    /// `model.Share.ID`.
    pub id: String,
    /// `model.Share.Downloadable`.
    pub downloadable: bool,
    /// `model.Share.Format`.
    pub format: String,
    /// `model.Share.MaxBitRate` — kbps.
    pub max_bit_rate: i64,
    /// `model.Share.ResourceType`.
    pub resource_type: String,
    /// `model.Share.ResourceIDs` — comma-separated, first element wins.
    pub resource_ids: String,
    /// `model.Share.Tracks`.
    pub tracks: Vec<ArchiveMediaFile>,
}

impl ArchiveShare {
    /// `Share.CoverArtID` (`model/share.go:35-54`). `cover_pick` is the index
    /// Go's `random.Int64N(len(tracks))` would have produced; the track arm is
    /// only reached for a resource type with no artwork of its own.
    #[must_use]
    pub fn cover_art_id(&self, cover_pick: usize, options: &ArchiverOptions) -> ArtworkId {
        let first = self.resource_ids.split(',').next().unwrap_or("");
        match self.resource_type.as_str() {
            "album" => album_artwork_id(first, ""),
            "playlist" => playlist_artwork_id(first, ""),
            "artist" => artist_artwork_id(first),
            _ => {
                if self.tracks.is_empty() {
                    return ArtworkId::none();
                }
                self.tracks[cover_pick % self.tracks.len()].cover_art_id(options)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// collaborators
// ---------------------------------------------------------------------------

/// The resolved transcode parameters the archiver consumes; `Format` also names
/// the zip entry (`core/archiver.go:169-178`).
pub use crate::types::Request as TranscodeRequest;

/// Failure arms of `addFileToZip`. Go distinguishes exactly one of them
/// (`errors.Is(err, stream.ErrTooManyTranscodes)`) and logs the rest
/// (`core/archiver.go:267-301`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SourceError {
    /// `stream.ErrTooManyTranscodes` (`core/stream/limiter.go:14`): the
    /// concurrency cap refused the transcode, which aborts the archive.
    #[error("too many concurrent transcodes")]
    TooManyTranscodes,
    /// Any other failure. Go logs it and keeps iterating the remaining tracks.
    #[error("stream open failed: {0}")]
    Other(String),
}

impl SourceError {
    /// `errors.Is(err, stream.ErrTooManyTranscodes)`.
    #[must_use]
    pub fn is_too_many_transcodes(&self) -> bool {
        matches!(self, SourceError::TooManyTranscodes)
    }
}

impl From<TooManyTranscodes> for SourceError {
    fn from(_: TooManyTranscodes) -> Self {
        SourceError::TooManyTranscodes
    }
}

/// A byte stream to copy into the zip entry — Go's `io.ReadCloser`, from
/// `MediaStreamer.NewStream` or `os.Open` (`core/archiver.go:267-285`).
pub trait ByteStream {
    /// Read up to `max` bytes; `None` is EOF. Chunk size does not affect the
    /// bytes written, so `ZIP_COPY_CHUNK_BYTES` matches Go's `io.Copy`.
    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, io::Result<Option<Vec<u8>>>>;

    /// Close the stream. Go logs a close failure only at debug level
    /// (`core/archiver.go:281-285`); the port hands it to the caller, which
    /// ignores it.
    fn close<'a>(&'a mut self) -> BoxFuture<'a, io::Result<()>>;
}

/// In-memory stream: the port of Go's `io.NopCloser(strings.NewReader(...))`
/// used throughout `core/archiver_test.go`.
#[derive(Debug, Default)]
pub struct CursorStream {
    data: Vec<u8>,
    pos: usize,
}

impl CursorStream {
    /// Wrap `data` as a stream that reads it in order.
    #[must_use]
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        CursorStream {
            data: data.into(),
            pos: 0,
        }
    }
}

impl ByteStream for CursorStream {
    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            if self.pos >= self.data.len() {
                return Ok(None);
            }
            let end = (self.pos + max).min(self.data.len());
            let chunk = self.data[self.pos..end].to_vec();
            self.pos = end;
            Ok(Some(chunk))
        })
    }

    fn close<'a>(&'a mut self) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { Ok(()) })
    }
}

/// `tokio::fs::File` as a [`ByteStream`] — Go's `os.Open(path)` in
/// `addFileToZip` (`core/archiver.go:272-276`).
#[derive(Debug)]
pub struct FileByteStream {
    file: tokio::fs::File,
}

impl FileByteStream {
    /// Open an **already-authorized** path; see the module trust boundary.
    pub async fn open(path: impl AsRef<std::path::Path>) -> io::Result<FileByteStream> {
        Ok(FileByteStream {
            file: tokio::fs::File::open(path).await?,
        })
    }
}

impl ByteStream for FileByteStream {
    fn read_chunk<'a>(&'a mut self, max: usize) -> BoxFuture<'a, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; max];
            let n = self.file.read(&mut buf).await?;
            if n == 0 {
                return Ok(None);
            }
            buf.truncate(n);
            Ok(Some(buf))
        })
    }

    fn close<'a>(&'a mut self) -> BoxFuture<'a, io::Result<()>> {
        // tokio's File closes on drop; an explicit close is a no-op.
        Box::pin(async move { Ok(()) })
    }
}

/// `stream.MediaStreamer.NewStream` (`core/stream/media_streamer.go:25-27`),
/// used only for the transcoded arm — the raw arm is the archiver's own open.
pub trait TranscodeStream {
    /// Open a transcoded stream for `mf` under `req`.
    fn new_stream<'a>(
        &'a mut self,
        mf: &'a ArchiveMediaFile,
        req: &'a Request,
    ) -> BoxFuture<'a, Result<Box<dyn ByteStream + Send>, SourceError>>;
}

/// A streamer that refuses every request: wiring that has no ffmpeg yet. Go
/// always constructs the archiver with a real streamer (`core/archiver.go:38-40`).
#[derive(Debug, Default)]
pub struct NoTranscodeStream;

impl TranscodeStream for NoTranscodeStream {
    fn new_stream<'a>(
        &'a mut self,
        _mf: &'a ArchiveMediaFile,
        _req: &'a Request,
    ) -> BoxFuture<'a, Result<Box<dyn ByteStream + Send>, SourceError>> {
        Box::pin(async move { Err(SourceError::Other("transcoding not wired".to_string())) })
    }
}

/// Cover-art read arms (`core/archiver.go:321-325`): `artwork.ErrUnavailable`
/// and `model.ErrNotFound` are debug-level "no cover" outcomes; anything else is
/// a warning that also skips the entry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoverArtError {
    /// `artwork.ErrUnavailable` (`core/artwork/artwork.go:22`).
    #[error("artwork unavailable")]
    Unavailable,
    /// `model.ErrNotFound`.
    #[error("not found")]
    NotFound,
    /// Any other error.
    #[error("cover art error: {0}")]
    Other(String),
}

impl CoverArtError {
    /// `errors.Is(err, artwork.ErrUnavailable) || errors.Is(err, model.ErrNotFound)`.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        matches!(self, CoverArtError::Unavailable | CoverArtError::NotFound)
    }
}

/// `artwork.Artwork.Get(ctx, artID, size, square)`
/// (`core/artwork/artwork.go:43`), buffered. `elevated` replaces Go's
/// `request.WithUser(ctx, model.User{IsAdmin: true})` on the share cover read
/// (`core/archiver.go:187-189`).
pub trait CoverArt {
    /// Read the artwork whole, at `size` pixels, optionally squared, optionally
    /// with an elevated (admin) viewer.
    fn get<'a>(
        &'a mut self,
        art_id: &'a ArtworkId,
        size: i32,
        square: bool,
        elevated: bool,
    ) -> BoxFuture<'a, Result<Vec<u8>, CoverArtError>>;
}

/// `stream.TranscodeDecider.ResolveRequest(ctx, mf, format, bitRate, 0)`
/// (`core/stream/decider.go:25`). `format`/`bit_rate` are the *requested*
/// values; Go's `resolveRequest` short-circuits them when raw
/// (`core/archiver.go:257-262`).
pub trait RequestResolver {
    /// Resolve the requested format/bitrate into the parameters to stream with.
    fn resolve(&self, mf: &ArchiveMediaFile, format: &str, bit_rate: i64) -> Request;
}

/// Resolver answering `{format, bit_rate}` verbatim — Go's `fakeDecider`
/// (`core/archiver_test.go:465-476`) and the effective legacy answer. Real wiring
/// adapts [`crate::decider::TranscodeDecider`] through
/// [`crate::legacy_client::resolve_request`].
#[derive(Debug, Default)]
pub struct LegacyResolver;

impl RequestResolver for LegacyResolver {
    fn resolve(&self, _mf: &ArchiveMediaFile, format: &str, bit_rate: i64) -> Request {
        Request {
            format: format.to_string(),
            bit_rate,
            ..Request::default()
        }
    }
}

/// `model.MediaFileRepository.GetAll(ctx, QueryOptions{Filters, Sort})`
/// (`core/archiver.go:66`).
pub trait TrackRepo {
    /// Load the tracks for `query`, in Go's query order.
    fn get_all<'a>(
        &'a mut self,
        query: &'a TrackQuery,
    ) -> BoxFuture<'a, Result<Vec<ArchiveMediaFile>, RepoError>>;
}

/// `model.PlaylistRepository.GetWithTracks(ctx, id, true, false)`
/// (`core/archiver.go:189-190`).
pub trait PlaylistRepo {
    /// Load a playlist with its tracks; the archiver always asks for
    /// `refresh_smart_playlists = true`, `include_missing = false`.
    fn get_with_tracks<'a>(
        &'a mut self,
        id: &'a str,
        refresh_smart_playlists: bool,
        include_missing: bool,
    ) -> BoxFuture<'a, Result<ArchivePlaylist, RepoError>>;
}

/// `model.DataStore` narrowed to the two repositories the archiver touches
/// (`core/archiver.go:46`).
pub trait DataSource: TrackRepo + PlaylistRepo {}

impl<T> DataSource for T where T: TrackRepo + PlaylistRepo {}

/// Repository failures, which Go logs and returns
/// (`core/archiver.go:66-68,170-172`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("repository error: {message}")]
pub struct RepoError {
    /// The message Go would log alongside the error.
    pub message: String,
}

/// Everything the archiver reads from Go's global config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiverOptions {
    /// `conf.Server.Subsonic.AppendAlbumVersion` (Go default true,
    /// `conf/configuration.go:1075`): whether the album-version tag is part of
    /// the folder name.
    pub append_album_version: bool,
    /// `conf.Server.EnableMediaFileCoverArt` (Go default true,
    /// `conf/configuration.go:1003`): whether a track's own embedded artwork may
    /// be used for a share cover (`model/mediafile.go:137-144`).
    pub enable_media_file_cover_art: bool,
}

/// Go's defaults: both flags default to `true` in `conf/configuration.go`.
impl Default for ArchiverOptions {
    fn default() -> Self {
        ArchiverOptions {
            append_album_version: true,
            enable_media_file_cover_art: true,
        }
    }
}

/// The scalar arguments of [`Archiver::zip_media_files`], mirroring Go's
/// `zipMediaFiles(ctx, id, name, format, bitrate, out, mfs, coverCtx, coverArt,
/// addM3U)` parameter list (`core/archiver.go:204`).
#[derive(Debug, Clone, Copy)]
pub struct ZipMediaFiles<'a> {
    /// Go's `id` — the log/context identifier.
    pub id: &'a str,
    /// Go's `name`: the playlist name, or the share id for a share.
    pub name: &'a str,
    /// Requested format (`""`/`"raw"` means no transcoding).
    pub format: &'a str,
    /// Requested bitrate in kbps.
    pub bitrate: i64,
    /// `model.MediaFiles` to archive.
    pub mfs: &'a [ArchiveMediaFile],
    /// Cover to add at the archive root; `ArtworkId::none()` adds nothing.
    pub cover_art: &'a ArtworkId,
    /// Append the `.m3u` entry (playlist only).
    pub add_m3u: bool,
    /// Read the cover with an elevated (admin) viewer, as shares do.
    pub elevated_cover: bool,
}

/// Errors [`Archiver`] can return. Go returns the underlying error verbatim in
/// each arm; the port wraps it so callers can distinguish the limiter, which is
/// the only arm that aborts mid-archive.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArchiverError {
    /// `model.ErrNotAuthorized` — a share that is not downloadable
    /// (`core/archiver.go:183-185`).
    #[error("not authorized")]
    NotAuthorized,
    /// Repository load failure.
    #[error(transparent)]
    Repo(#[from] RepoError),
    /// Stream open or copy failure; `TooManyTranscodes` aborts, others do not.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// Zip writer failure.
    #[error(transparent)]
    Zip(#[from] ZipError),
    /// Filesystem failure on the raw arm.
    #[error("file error: {0}")]
    Io(String),
}

impl ArchiverError {
    /// True for the limiter arm — the only one that aborts the archive.
    #[must_use]
    pub fn is_too_many_transcodes(&self) -> bool {
        matches!(self, ArchiverError::Source(SourceError::TooManyTranscodes))
    }
}

impl From<io::Error> for ArchiverError {
    fn from(err: io::Error) -> Self {
        ArchiverError::Io(err.to_string())
    }
}

/// `createZipWriter` (`core/archiver.go:105-113`): the comment Go writes for a
/// transcoded download is `"Downloaded from Navidrome, transcoded to <fmt> <n>bps"`.
#[must_use]
pub fn create_zip_writer<W: AsyncSink>(out: W, format: &str, bitrate: i64) -> ZipWriter<W> {
    let mut z = ZipWriter::new(out);
    let mut comment = DEFAULT_ZIP_COMMENT.to_string();
    if format != "raw" && !format.is_empty() {
        comment = format!("{comment}, transcoded to {format} {bitrate}bps");
    }
    // Go ignores this error too (`_ = z.SetComment(comment)`); it can only fail
    // on a >65535-byte comment.
    let _ = z.set_comment(comment);
    z
}

/// Port of the unexported `core.archiver` (`core/archiver.go:42-48`). Go's DI
/// becomes explicit construction; there is no `shares` dependency because
/// `ZipShare` takes the already-loaded share.
#[derive(Debug)]
pub struct Archiver<S, R, C, D> {
    /// `ms` — the transcoding arm of `addFileToZip`.
    pub streams: S,
    /// `decider`.
    pub resolver: R,
    /// `artwork`.
    pub covers: C,
    /// `ds`.
    pub store: D,
    /// Config the zip layout depends on.
    pub options: ArchiverOptions,
    /// Clock for cover-art timestamps (Go's `time.Now()`); injectable so tests
    /// are deterministic.
    pub now: fn() -> i64,
}

impl<S, R, C, D> Archiver<S, R, C, D>
where
    S: TranscodeStream,
    R: RequestResolver,
    C: CoverArt,
    D: DataSource,
{
    /// `NewArchiver(ms, decider, ds, shares, artwork)` (`core/archiver.go:38-40`)
    /// with the config the layout reads. `now` supplies Unix seconds for the
    /// cover entries Go stamps with `time.Now()`.
    #[must_use]
    pub fn new(
        streams: S,
        resolver: R,
        covers: C,
        store: D,
        options: ArchiverOptions,
        now: fn() -> i64,
    ) -> Self {
        Archiver {
            streams,
            resolver,
            covers,
            store,
            options,
            now,
        }
    }

    /// `ZipAlbum` (`core/archiver.go:50-52`).
    pub async fn zip_album<W: AsyncSink>(
        &mut self,
        z: &mut ZipWriter<W>,
        id: &str,
        format: &str,
        bitrate: i64,
    ) -> Result<(), ArchiverError> {
        self.zip_albums(
            z,
            id,
            format,
            bitrate,
            TrackFilter::AlbumId(id.to_string()),
            ArtworkId::none(),
        )
        .await
    }

    /// `ZipArtist` (`core/archiver.go:54-62`): the root cover is the artist's
    /// `CoverArtID()`, added after every album's own cover.
    pub async fn zip_artist<W: AsyncSink>(
        &mut self,
        z: &mut ZipWriter<W>,
        id: &str,
        format: &str,
        bitrate: i64,
    ) -> Result<(), ArchiverError> {
        self.zip_albums(
            z,
            id,
            format,
            bitrate,
            TrackFilter::AlbumArtistParticipant {
                artist_id: id.to_string(),
            },
            artist_artwork_id(id),
        )
        .await
    }

    /// `zipAlbums` (`core/archiver.go:64-103`).
    pub async fn zip_albums<W: AsyncSink>(
        &mut self,
        z: &mut ZipWriter<W>,
        id: &str,
        format: &str,
        bitrate: i64,
        filter: TrackFilter,
        root_art: ArtworkId,
    ) -> Result<(), ArchiverError> {
        let mfs = self
            .store
            .get_all(&TrackQuery {
                filter,
                sort: "album".to_string(),
            })
            .await?;

        let albums = group_ordered(&mfs, |mf| mf.album_id.clone());
        let folders = album_folders(&albums, self.options.append_album_version);
        for album in &albums {
            let discs = album_count_discs(album);
            let is_multi_disc = discs > 1;
            // Go reads `folders[album[0].AlbumID]`, whose zero value is "".
            // `album_folders` always fills the map (the album-id disambiguator
            // is unique by construction), so the empty fallback is Go's
            // unreachable arm, ported as written.
            let folder = folders.get(&album[0].album_id).cloned().unwrap_or_default();
            for mf in album.iter().copied() {
                let req = self.resolve_request(mf, format, bitrate);
                let file = self.album_filename(mf, &req.format, is_multi_disc, &folder);
                if let Err(err) = self.add_file_to_zip(z, mf, &req, &file).await {
                    if err.is_too_many_transcodes() {
                        // Stop iterating: continuing would only rack up more
                        // rejections. Close finalizes the tracks already
                        // written; the rejected one is absent because the entry
                        // header is written after the source is opened.
                        // `core/archiver.go:84-92`.
                        let _ = z.close().await;
                        return Err(err);
                    }
                    // Go logs and continues (`core/archiver.go:89-92`).
                }
            }
            // After the tracks, so a slow artwork lookup does not delay the
            // first bytes (`core/archiver.go:94`).
            self.add_cover_art_to_zip(z, &album[0].album_cover_art_id(), &folder, false)
                .await;
        }
        self.add_cover_art_to_zip(z, &root_art, "", false).await;
        let _ = id;
        z.close().await.map_err(ArchiverError::from)
    }

    /// `ZipShare` (`core/archiver.go:182-191`). `cover_pick` selects the track
    /// Go would have chosen at random for a share whose resource has no artwork
    /// of its own; for `album`/`playlist`/`artist` resources it is unused.
    pub async fn zip_share<W: AsyncSink>(
        &mut self,
        z: &mut ZipWriter<W>,
        share: &ArchiveShare,
        cover_pick: usize,
    ) -> Result<(), ArchiverError> {
        if !share.downloadable {
            return Err(ArchiverError::NotAuthorized);
        }
        // The share is the authorization (as in the public image handler): an
        // anonymous lookup would hide a private playlist. Only the cover read is
        // elevated (`core/archiver.go:187-189`).
        let cover_art = share.cover_art_id(cover_pick, &self.options);
        self.zip_media_files(
            z,
            &ZipMediaFiles {
                id: &share.id,
                name: &share.id,
                format: &share.format,
                bitrate: share.max_bit_rate,
                mfs: &share.tracks,
                cover_art: &cover_art,
                add_m3u: false,
                elevated_cover: true,
            },
        )
        .await
    }

    /// `ZipPlaylist` (`core/archiver.go:193-202`).
    pub async fn zip_playlist<W: AsyncSink>(
        &mut self,
        z: &mut ZipWriter<W>,
        id: &str,
        format: &str,
        bitrate: i64,
    ) -> Result<(), ArchiverError> {
        let pls = self.store.get_with_tracks(id, true, false).await?;
        let cover_art = pls.cover_art_id();
        self.zip_media_files(
            z,
            &ZipMediaFiles {
                id,
                name: &pls.name,
                format,
                bitrate,
                mfs: &pls.tracks,
                cover_art: &cover_art,
                add_m3u: true,
                elevated_cover: false,
            },
        )
        .await
    }

    /// `zipMediaFiles` (`core/archiver.go:294-247`). The scalar arguments are
    /// bundled in [`ZipMediaFiles`] to keep the signature readable; `id` is
    /// Go's log/context argument and is carried for the same reason.
    pub async fn zip_media_files<W: AsyncSink>(
        &mut self,
        z: &mut ZipWriter<W>,
        opts: &ZipMediaFiles<'_>,
    ) -> Result<(), ArchiverError> {
        let ZipMediaFiles {
            id,
            name,
            format,
            bitrate,
            mfs,
            cover_art,
            add_m3u,
            elevated_cover,
        } = *opts;
        let mut zipped: Vec<ArchiveMediaFile> = Vec::with_capacity(mfs.len());
        for (idx, mf) in mfs.iter().enumerate() {
            let req = self.resolve_request(mf, format, bitrate);
            let file = self.playlist_filename(mf, &req.format, idx);
            if let Err(err) = self.add_file_to_zip(z, mf, &req, &file).await {
                if err.is_too_many_transcodes() {
                    // Abort the whole archive: continuing would silently emit
                    // empty entries, since headers are already written
                    // (`core/archiver.go:211-216`).
                    let _ = z.close().await;
                    return Err(err);
                }
            }
            // Go assigns `mf.Path = file` unconditionally, so a track whose
            // bytes failed to copy still appears in the M3U.
            let mut zipped_mf = mf.clone();
            zipped_mf.path = file;
            zipped.push(zipped_mf);
        }
        self.add_cover_art_to_zip(z, cover_art, "", elevated_cover)
            .await;

        if add_m3u && !mfs.is_empty() {
            let pls_name = sanitize_filename(name);
            let content = to_m3u8(&zipped, &pls_name, false);
            let header = ZipFileHeader {
                name: format!("{pls_name}.m3u"),
                modified: mfs[0].updated_at,
                method: ZipMethod::Store,
                non_utf8: false,
            };
            let mut entry = z.create_header(&header).await?;
            entry.write(content.as_bytes()).await?;
            entry.close().await?;
        }

        let _ = id;
        z.close().await.map_err(ArchiverError::from)
    }

    /// `resolveRequest` (`core/archiver.go:257-262`).
    #[must_use]
    pub fn resolve_request(&self, mf: &ArchiveMediaFile, format: &str, bitrate: i64) -> Request {
        if format.is_empty() || format == "raw" {
            return Request {
                format: "raw".to_string(),
                ..Request::default()
            };
        }
        self.resolver.resolve(mf, format, bitrate)
    }

    /// `albumFilename` (`core/archiver.go:169-178`).
    #[must_use]
    pub fn album_filename(
        &self,
        mf: &ArchiveMediaFile,
        format: &str,
        is_multi_disc: bool,
        folder: &str,
    ) -> String {
        let (_, file) = file_path_split(&mf.path);
        let mut file = file;
        if format != "raw" {
            file = trimmed_suffix(&file, &mf.suffix);
            file.push_str(format);
        }
        if is_multi_disc {
            file = format!("Disc {:02}/{file}", mf.disc_number);
        }
        format!("{folder}/{file}")
    }

    /// `playlistFilename` (`core/archiver.go:249-255`).
    #[must_use]
    pub fn playlist_filename(&self, mf: &ArchiveMediaFile, format: &str, idx: usize) -> String {
        let mut ext = mf.suffix.clone();
        if !format.is_empty() && format != "raw" {
            ext = format.to_string();
        }
        format!(
            "{:02} - {} - {}.{}",
            idx + 1,
            sanitize_filename(&mf.artist),
            sanitize_filename(&mf.title),
            ext
        )
    }

    /// `addFileToZip` (`core/archiver.go:264-304`). The source is opened
    /// **before** the entry header so a refusal cannot leave an empty entry.
    pub async fn add_file_to_zip<W: AsyncSink>(
        &mut self,
        z: &mut ZipWriter<W>,
        mf: &ArchiveMediaFile,
        req: &Request,
        filename: &str,
    ) -> Result<(), ArchiverError> {
        let path = mf.absolute_path();
        let mut source: Box<dyn ByteStream + Send> = if req.format != "raw" {
            self.streams
                .new_stream(mf, req)
                .await
                .map_err(ArchiverError::Source)?
        } else {
            Box::new(FileByteStream::open(&path).await?)
        };

        let header = ZipFileHeader {
            name: filename.to_string(),
            modified: mf.updated_at,
            method: ZipMethod::Store,
            non_utf8: false,
        };
        let mut entry = z.create_header(&header).await?;
        let mut copy_result = Ok(());
        loop {
            match source.read_chunk(ZIP_COPY_CHUNK_BYTES).await {
                Ok(Some(chunk)) => {
                    if let Err(err) = entry.write(&chunk).await {
                        copy_result = Err(ArchiverError::from(err));
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    copy_result = Err(ArchiverError::Io(err.to_string()));
                    break;
                }
            }
        }
        let _ = source.close().await;
        copy_result?;
        entry.close().await.map_err(ArchiverError::from)
    }

    /// `addCoverArtToZip` (`core/archiver.go:307-338`): errors are logged, never
    /// returned, and a failed read leaves no entry at all.
    pub async fn add_cover_art_to_zip<W: AsyncSink>(
        &mut self,
        z: &mut ZipWriter<W>,
        art_id: &ArtworkId,
        dir: &str,
        elevated: bool,
    ) {
        if art_id.id.is_empty() {
            return;
        }
        let data = match self
            .covers
            .get(art_id, ARCHIVE_COVER_ART_SIZE, false, elevated)
            .await
        {
            Ok(data) => data,
            Err(_) => return,
        };
        let ext = cover_art_extension(&data);
        if ext.is_empty() {
            return;
        }
        let header = ZipFileHeader {
            name: path_join(dir, &format!("folder.{ext}")),
            modified: Some((self.now)()),
            method: ZipMethod::Store,
            non_utf8: false,
        };
        let Ok(mut entry) = z.create_header(&header).await else {
            return;
        };
        if entry.write(&data).await.is_err() {
            return;
        }
        let _ = entry.close().await;
    }

    /// `readCoverArt` (`core/archiver.go:340-347`) — exposed so a caller can
    /// reuse the same size/permission contract outside an archive.
    pub async fn read_cover_art(
        &mut self,
        art_id: &ArtworkId,
        elevated: bool,
    ) -> Result<Vec<u8>, CoverArtError> {
        self.covers
            .get(art_id, ARCHIVE_COVER_ART_SIZE, false, elevated)
            .await
    }
}

/// `slice.GroupOrdered` (`utils/slice/slice.go`) over borrowed rows, keyed by
/// `key_func`, preserving first-seen order.
#[must_use]
pub fn group_ordered<'a, T>(items: &'a [T], key_func: impl Fn(&T) -> String) -> Vec<Vec<&'a T>> {
    let mut groups: Vec<Vec<&'a T>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for item in items {
        let key = key_func(item);
        let slot = *index.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[slot].push(item);
    }
    groups
}

/// Number of distinct disc numbers in an album — Go's
/// `len(slice.Group(album, func(mf) int { return mf.DiscNumber }))`
/// (`core/archiver.go:76`).
#[must_use]
pub fn album_count_discs(album: &[&ArchiveMediaFile]) -> usize {
    let mut discs: Vec<i32> = album.iter().map(|mf| mf.disc_number).collect();
    discs.sort_unstable();
    discs.dedup();
    discs.len()
}

/// The disambiguators tried in order (`albumDisambiguators`,
/// `core/archiver.go:117-131`): the first field whose values are distinct across
/// the clashing albums wins.
#[must_use]
pub fn album_disambiguators(mf: &ArchiveMediaFile) -> Vec<String> {
    let year = if mf.release_year != 0 {
        mf.release_year
    } else {
        mf.year
    };
    vec![
        mf.tag_first("albumversion").to_string(),
        if year != 0 {
            year.to_string()
        } else {
            String::new()
        },
        mf.mbz_album_type.clone(),
        mf.tag_first("recordlabel").to_string(),
        mf.catalog_num.clone(),
        mf.album_id.chars().take(6).collect(),
        mf.album_id.clone(),
    ]
}

/// `albumFolders` (`core/archiver.go:135-167`): each album id maps to its zip
/// folder; albums whose names sanitize to the same folder get `" [suffix]"` from
/// the first disambiguator that tells them all apart.
#[must_use]
pub fn album_folders(
    albums: &[Vec<&ArchiveMediaFile>],
    append_album_version: bool,
) -> BTreeMap<String, String> {
    let mut by_name: Vec<(String, Vec<&ArchiveMediaFile>)> = Vec::new();
    for album in albums {
        let name = sanitize_filename(&album[0].full_album_name(append_album_version));
        if let Some(slot) = by_name.iter_mut().find(|(n, _)| *n == name) {
            slot.1.push(album[0]);
        } else {
            by_name.push((name, vec![album[0]]));
        }
    }

    let mut folders: BTreeMap<String, String> = BTreeMap::new();
    for (name, group) in by_name {
        if group.len() == 1 {
            folders.insert(group[0].album_id.clone(), name);
            continue;
        }
        let mut resolved = false;
        for field_index in 0..album_disambiguators(group[0]).len() {
            let mut ids: BTreeMap<String, String> = BTreeMap::new(); // suffix -> album id
            let mut clash = false;
            for mf in &group {
                let suffix = sanitize_filename(&album_disambiguators(mf)[field_index]);
                if ids.contains_key(&suffix) {
                    clash = true;
                    break;
                }
                ids.insert(suffix, mf.album_id.clone());
            }
            if clash {
                continue;
            }
            for (suffix, album_id) in ids {
                let mut folder = name.clone();
                if !suffix.is_empty() {
                    folder = format!("{name} [{suffix}]");
                }
                folders.insert(album_id, folder);
            }
            resolved = true;
            break;
        }
        if !resolved {
            // Go's loop always terminates on the album-id disambiguator, which
            // is unique per album; kept as a defensive arm.
            for mf in &group {
                folders.insert(mf.album_id.clone(), name.clone());
            }
        }
    }
    folders
}

/// `strings.TrimSuffix(file, mf.Suffix)` (`core/archiver.go:172`).
fn trimmed_suffix(file: &str, suffix: &str) -> String {
    if !suffix.is_empty() && file.ends_with(suffix) {
        file[..file.len() - suffix.len()].to_string()
    } else {
        file.to_string()
    }
}

/// `path.Join(dir, "folder.ext")` (`core/archiver.go:327`): an empty dir yields
/// the bare name.
#[must_use]
pub fn path_join(dir: &str, name: &str) -> String {
    // `path.Join` (`path/path.go:166-173`), the same skip-then-clean rule as
    // `filepath.Join` on unix. Go uses it for `dir/folder.<ext>`
    // (`core/archiver.go:327`).
    file_path_join(&[dir, name])
}

/// `model.MediaFiles.ToM3U8(title, absolutePaths)`
/// (`model/mediafile.go:524-537`).
///
/// Go formats the duration with `%.f`, which rounds **half to even**
/// (`strconv`'s round-to-nearest tie-breaking). Rust's `{:.0}` does the same,
/// which is why `#EXTINF:2` for 1.5s and 2.5s matches Go; the oracle in
/// `tests/data/go_zip/manifest.json` pins the whole table.
#[must_use]
pub fn to_m3u8(mfs: &[ArchiveMediaFile], title: &str, absolute_paths: bool) -> String {
    let mut out = String::new();
    out.push_str("#EXTM3U\n");
    out.push_str(&format!("#PLAYLIST:{title}\n"));
    for mf in mfs {
        out.push_str(&format!(
            "#EXTINF:{:.0},{} - {}\n",
            mf.duration, mf.artist, mf.title
        ));
        if absolute_paths {
            out.push_str(&mf.absolute_path());
        } else {
            out.push_str(&mf.path);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn crc32_matches_go() {
        assert_eq!(crc32_ieee(b""), 0);
        assert_eq!(crc32_ieee(b"test"), 0xd87f_7e0c);
        assert_eq!(crc32_ieee(b"AAAA"), 0x9b0d_08f1);
    }

    #[test]
    fn ms_dos_time_matches_go_for_utc() {
        // 2021-07-14T12:34:56Z → the oracle's 21230 / 25692.
        assert_eq!(ms_dos_time(1_626_266_096), (21230, 25692));
        assert_eq!(ms_dos_time(0), (60449, 0));
        assert_eq!(ms_dos_time(-1), (60319, 49021));
    }

    #[test]
    fn detect_utf8_requires_the_flag_for_cjk_tilde_and_backslash() {
        assert!(!detect_utf8("Album/01.mp3").1);
        assert!(detect_utf8("Album 東京/01.mp3").1);
        assert!(detect_utf8("Hits~/02.mp3").1);
        assert!(detect_utf8(r"A\B/03.mp3").1);
    }

    #[test]
    fn path_clean_matches_go() {
        assert_eq!(
            path_clean("/music/Album/../Other/01.mp3"),
            "/music/Other/01.mp3"
        );
        assert_eq!(path_clean("/music"), "/music");
        assert_eq!(file_path_join(&["/music", ""]), "/music");
        assert_eq!(file_path_join(&["", ""]), "");
        assert_eq!(
            file_path_join(&["/music/", "/Album/01.mp3"]),
            "/music/Album/01.mp3"
        );
        assert_eq!(file_path_join(&["/music", ".."]), "/");
        assert_eq!(file_path_join(&["music", "a/../b.mp3"]), "music/b.mp3");
    }

    #[test]
    fn file_path_split_splits_at_the_last_separator() {
        assert_eq!(
            file_path_split("test_data/01 - track1.mp3").1,
            "01 - track1.mp3"
        );
        assert_eq!(
            file_path_split("01.mp3"),
            (String::new(), "01.mp3".to_string())
        );
    }

    #[test]
    fn append_suffix_wraps_unbracketed_suffixes() {
        assert_eq!(
            append_suffix("Greatest Hits", "Original"),
            "Greatest Hits (Original)"
        );
        assert_eq!(
            append_suffix("Greatest Hits", "[Deluxe]"),
            "Greatest Hits [Deluxe]"
        );
        assert_eq!(
            append_suffix("Greatest Hits", "  {Live}  "),
            "Greatest Hits {Live}"
        );
    }

    #[test]
    fn m3u8_rounds_half_to_even_like_go() {
        let mf = |d: f32| ArchiveMediaFile {
            duration: d,
            artist: "Artist".to_string(),
            title: "Title".to_string(),
            path: "p.mp3".to_string(),
            ..ArchiveMediaFile::default()
        };
        let out = to_m3u8(&[mf(1.5), mf(2.5), mf(200.5)], "T", false);
        assert!(out.contains("#EXTINF:2,Artist - Title"));
        assert!(out.contains("#EXTINF:200,Artist - Title"));
    }

    #[test]
    fn artwork_id_renders_like_go() {
        assert_eq!(album_artwork_id("1", "").render(), "al-1");
        assert_eq!(album_artwork_id("1", "abc123").render(), "al-1_abc123");
        assert_eq!(artist_artwork_id("1").render(), "ar-1");
        assert_eq!(ArtworkId::none().render(), "");
        assert_eq!(disc_artwork_id("al-1", 2), "al-1:2");
    }

    #[test]
    fn zip_comment_follows_go() {
        assert_eq!(
            create_zip_writer(Vec::new(), "raw", 128).comment(),
            DEFAULT_ZIP_COMMENT
        );
        assert_eq!(
            create_zip_writer(Vec::new(), "", 128).comment(),
            DEFAULT_ZIP_COMMENT
        );
        assert_eq!(
            create_zip_writer(Vec::new(), "mp3", 128).comment(),
            "Downloaded from Navidrome, transcoded to mp3 128bps"
        );
    }

    #[tokio::test]
    async fn deflate_is_refused() {
        let mut z = ZipWriter::new(Vec::new());
        let header = ZipFileHeader {
            name: "a.mp3".to_string(),
            modified: None,
            method: ZipMethod::Deflate,
            non_utf8: false,
        };
        assert_eq!(
            z.create_header(&header).await.err(),
            Some(ZipError::UnsupportedMethod)
        );
    }

    #[tokio::test]
    async fn writing_after_close_reports_go_error() {
        let mut z = ZipWriter::new(Vec::new());
        let header = ZipFileHeader::stored("a.mp3");

        let mut e = z.create_header(&header).await.unwrap();
        e.write(b"x").await.unwrap();
        e.close().await.unwrap();
        assert_eq!(e.write(b"y").await.err(), Some(ZipError::WriteToClosedFile));
        z.close().await.unwrap();
        assert_eq!(z.close().await.err(), Some(ZipError::ClosedTwice));
    }
}
