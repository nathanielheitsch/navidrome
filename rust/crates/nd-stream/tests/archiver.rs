//! #15 archiver tests: the zip-layout contract pinned against **Go's own
//! `archive/zip`** (oracle in `tests/data/go_zip/`, harness in
//! `tests/testdata/go_zip/main.go`), plus the behavior cases ported from
//! `core/archiver_test.go`.
//!
//! Two layers, deliberately separate:
//!   1. **Byte goldens** — the Rust `ZipWriter` must emit the same bytes as Go
//!      for the headers the archiver writes. Deterministic archives are
//!      compared whole; archives containing Go's `time.Now()` cover entry are
//!      compared up to that entry's local header, then field by field
//!      (CRC, sizes, UTF-8 flag, extra field, offsets). The MS-DOS fields of a
//!      `time.Now()` entry are excluded on purpose: Go encodes the local wall
//!      clock there and the port encodes UTC (`src/archiver.rs` module docs).
//!   2. **Layout** — folder naming, album disambiguation, disc prefixes,
//!      playlist numbering, M3U content, cover placement, and the
//!      `ErrTooManyTranscodes` abort semantics from `core/archiver_test.go`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nd_stream::archiver::{
    absolute_path, album_artwork_id, album_count_discs, album_disambiguators, album_folders,
    append_suffix, cover_art_extension, crc32_ieee, create_zip_writer, ms_dos_time, path_clean,
    to_m3u8, ArchiveMediaFile, ArchivePlaylist, ArchiveShare, Archiver, ArchiverError,
    ArchiverOptions, ArtworkId, AsyncSink, BoxFuture, ByteStream, CoverArt, CoverArtError,
    CursorStream, LegacyResolver, PlaylistRepo, RepoError, SourceError, TrackFilter, TrackQuery,
    TrackRepo, TranscodeStream, ZipError, ZipFileHeader, ZipMethod, ZipWriter,
    ARCHIVE_COVER_ART_SIZE, DEFAULT_ZIP_COMMENT,
};
use nd_stream::types::Request;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// oracle fixtures
// ---------------------------------------------------------------------------

/// The Unix instant the goldens stamp deterministic entries with
/// (`2021-07-14T12:34:56Z`).
const FIXED_UNIX: i64 = 1_626_266_096;
/// The instant this test's injected clock returns for cover entries.
const NOW_UNIX: i64 = 1_626_266_096;

fn oracle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/go_zip")
}

fn manifest_path() -> PathBuf {
    oracle_dir().join("manifest.json")
}

fn load_manifest() -> Manifest {
    let text = std::fs::read_to_string(manifest_path())
        .unwrap_or_else(|err| panic!("read {}: {err}", manifest_path().display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse manifest: {err}"))
}

fn golden_bytes(name: &str) -> Vec<u8> {
    let path = oracle_dir().join(format!("{name}.zip"));
    std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length: {}", s.len());
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[derive(Debug, Deserialize)]
struct Manifest {
    fixed_unix: i64,
    goldens: Vec<Golden>,
    cover_ext_cases: Vec<CoverExtCase>,
    msdos_cases: Vec<MsDosCase>,
    crc32_cases: Vec<CrcCase>,
    join_cases: Vec<JoinCase>,
    m3u_duration_cases: Vec<M3uCase>,
}

#[derive(Debug, Deserialize)]
struct Golden {
    name: String,
    comment: String,
    zip_file: String,
    fixed_unix: i64,
    // Go marshals an empty `[]zip.File` as JSON null, so the empty golden has
    // no entries array at all.
    #[serde(default)]
    entries: Option<Vec<OracleEntry>>,
    #[serde(default)]
    deterministic_prefix_hex: String,
    bytes_hex: String,
    deterministic: bool,
}

#[derive(Debug, Deserialize)]
struct OracleEntry {
    name: String,
    method: u16,
    flags: u16,
    crc32: u32,
    comp_size: u64,
    uncomp_size: u64,
    mod_date: u16,
    mod_time: u16,
    extra_hex: String,
    local_offset: i64,
    data_hex: String,
    modified: String,
}

#[derive(Debug, Deserialize)]
struct CoverExtCase {
    name: String,
    payload_hex: String,
    content_type: String,
    ext: String,
}

#[derive(Debug, Deserialize)]
struct MsDosCase {
    unix: i64,
    date: u16,
    time: u16,
}

#[derive(Debug, Deserialize)]
struct CrcCase {
    input_hex: String,
    crc: u32,
}

#[derive(Debug, Deserialize)]
struct JoinCase {
    library: String,
    path: String,
    joined: String,
}

#[derive(Debug, Deserialize)]
struct M3uCase {
    duration: f32,
    line: String,
}

// ---------------------------------------------------------------------------
// minimal zip reader (test-side only, for layout assertions)
// ---------------------------------------------------------------------------

/// One entry as the archive recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadEntry {
    name: String,
    method: u16,
    flags: u16,
    crc: u32,
    uncomp_size: u64,
    mod_date: u16,
    mod_time: u16,
    extra: Vec<u8>,
    offset: u64,
    data: Vec<u8>,
}

/// Read the central directory and each entry's stored bytes. Method is always
/// `Store` here, which is all the archiver writes.
fn read_zip(bytes: &[u8]) -> Vec<ReadEntry> {
    let eocd_pos = find_eocd(bytes);
    let count = u16::from_le_bytes([bytes[eocd_pos + 8], bytes[eocd_pos + 9]]) as usize;
    let cd_offset = u32::from_le_bytes([
        bytes[eocd_pos + 16],
        bytes[eocd_pos + 17],
        bytes[eocd_pos + 18],
        bytes[eocd_pos + 19],
    ]) as usize;
    let comment_len = u16::from_le_bytes([bytes[eocd_pos + 20], bytes[eocd_pos + 21]]) as usize;
    let comment_end = eocd_pos + 22 + comment_len;
    assert_eq!(
        comment_end,
        bytes.len(),
        "EOCD comment must end the archive"
    );

    let mut p = cd_offset;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        assert_eq!(
            u32::from_le_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]),
            0x0201_4b50,
            "central directory signature"
        );
        let flags = u16::from_le_bytes([bytes[p + 8], bytes[p + 9]]);
        let method = u16::from_le_bytes([bytes[p + 10], bytes[p + 11]]);
        let mod_time = u16::from_le_bytes([bytes[p + 12], bytes[p + 13]]);
        let mod_date = u16::from_le_bytes([bytes[p + 14], bytes[p + 15]]);
        let crc = u32::from_le_bytes([bytes[p + 16], bytes[p + 17], bytes[p + 18], bytes[p + 19]]);
        let uncomp =
            u32::from_le_bytes([bytes[p + 24], bytes[p + 25], bytes[p + 26], bytes[p + 27]]);
        let name_len = u16::from_le_bytes([bytes[p + 28], bytes[p + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[p + 30], bytes[p + 31]]) as usize;
        let offset =
            u32::from_le_bytes([bytes[p + 42], bytes[p + 43], bytes[p + 44], bytes[p + 45]])
                as usize;
        let name = String::from_utf8(bytes[p + 46..p + 46 + name_len].to_vec()).unwrap();
        let extra = bytes[p + 46 + name_len..p + 46 + name_len + extra_len].to_vec();
        p += 46 + name_len + extra_len;

        // Local header at `offset`; stored data begins after name + extra.
        assert_eq!(
            u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3]
            ]),
            0x0403_4b50,
            "local file header signature for {name}"
        );
        let l_name_len = u16::from_le_bytes([bytes[offset + 26], bytes[offset + 27]]) as usize;
        let l_extra_len = u16::from_le_bytes([bytes[offset + 28], bytes[offset + 29]]) as usize;
        let data_start = offset + 30 + l_name_len + l_extra_len;
        let data = bytes[data_start..data_start + uncomp as usize].to_vec();
        entries.push(ReadEntry {
            name,
            method,
            flags,
            crc,
            uncomp_size: u64::from(uncomp),
            mod_date,
            mod_time,
            extra,
            offset: offset as u64,
            data,
        });
    }
    entries
}

fn find_eocd(bytes: &[u8]) -> usize {
    for i in (0..bytes.len()).rev() {
        if i + 4 <= bytes.len()
            && u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
                == 0x0605_4b50
        {
            return i;
        }
    }
    panic!("no end-of-central-directory record")
}

fn eocd_comment(bytes: &[u8]) -> String {
    let pos = find_eocd(bytes);
    let len = u16::from_le_bytes([bytes[pos + 20], bytes[pos + 21]]) as usize;
    String::from_utf8(bytes[pos + 22..pos + 22 + len].to_vec()).unwrap()
}

// ---------------------------------------------------------------------------
// test doubles
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeStore {
    tracks: Vec<ArchiveMediaFile>,
    playlist: Option<ArchivePlaylist>,
    queries: Vec<TrackQuery>,
    playlist_calls: Vec<(String, bool, bool)>,
    error: Option<String>,
}

impl TrackRepo for FakeStore {
    fn get_all<'a>(
        &'a mut self,
        query: &'a TrackQuery,
    ) -> BoxFuture<'a, Result<Vec<ArchiveMediaFile>, RepoError>> {
        Box::pin(async move {
            self.queries.push(query.clone());
            if let Some(message) = &self.error {
                return Err(RepoError {
                    message: message.clone(),
                });
            }
            Ok(self.tracks.clone())
        })
    }
}

impl PlaylistRepo for FakeStore {
    fn get_with_tracks<'a>(
        &'a mut self,
        id: &'a str,
        refresh: bool,
        include_missing: bool,
    ) -> BoxFuture<'a, Result<ArchivePlaylist, RepoError>> {
        Box::pin(async move {
            self.playlist_calls
                .push((id.to_string(), refresh, include_missing));
            self.playlist.clone().ok_or_else(|| RepoError {
                message: "playlist not found".to_string(),
            })
        })
    }
}

/// Records every request the archiver makes; `refuse_first` reproduces Go's
/// `ms.On("NewStream", ...).Return(nil, stream.ErrTooManyTranscodes).Once()`.
#[derive(Default)]
struct FakeStreams {
    payload: Vec<u8>,
    requests: Vec<(String, Request)>,
    refuse_first: usize,
    fail_first: usize,
}

impl TranscodeStream for FakeStreams {
    fn new_stream<'a>(
        &'a mut self,
        mf: &'a ArchiveMediaFile,
        req: &'a Request,
    ) -> BoxFuture<'a, Result<Box<dyn ByteStream + Send>, SourceError>> {
        Box::pin(async move {
            self.requests.push((mf.id.clone(), req.clone()));
            if self.requests.len() <= self.refuse_first {
                return Err(SourceError::TooManyTranscodes);
            }
            if self.requests.len() <= self.fail_first {
                return Err(SourceError::Other("boom".to_string()));
            }
            Ok(Box::new(CursorStream::new(self.payload.clone())) as Box<dyn ByteStream + Send>)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CoverRequest {
    id: String,
    size: i32,
    square: bool,
    elevated: bool,
}

#[derive(Default)]
struct FakeCovers {
    images: BTreeMap<String, Vec<u8>>,
    requests: Vec<CoverRequest>,
    error: Option<CoverArtError>,
}

impl CoverArt for FakeCovers {
    fn get<'a>(
        &'a mut self,
        art_id: &'a ArtworkId,
        size: i32,
        square: bool,
        elevated: bool,
    ) -> BoxFuture<'a, Result<Vec<u8>, CoverArtError>> {
        Box::pin(async move {
            self.requests.push(CoverRequest {
                id: art_id.render(),
                size,
                square,
                elevated,
            });
            if let Some(err) = &self.error {
                return Err(err.clone());
            }
            self.images
                .get(&art_id.render())
                .cloned()
                .ok_or(CoverArtError::Unavailable)
        })
    }
}

fn fixed_now() -> i64 {
    NOW_UNIX
}

/// `core/archiver_test.go` builds tracks with only the fields the archiver
/// reads; this is the same shape.
fn mf(path: &str, suffix: &str, album_id: &str, album: &str) -> ArchiveMediaFile {
    ArchiveMediaFile {
        id: path.to_string(),
        path: path.to_string(),
        suffix: suffix.to_string(),
        album_id: album_id.to_string(),
        album: album.to_string(),
        updated_at: Some(FIXED_UNIX),
        ..ArchiveMediaFile::default()
    }
}

fn jpeg() -> Vec<u8> {
    b"\xff\xd8\xff\xe0 fake jpeg".to_vec()
}

fn png() -> Vec<u8> {
    b"\x89PNG\r\n\x1a\n fake png".to_vec()
}

fn archiver(
    store: FakeStore,
    streams: FakeStreams,
    covers: FakeCovers,
    options: ArchiverOptions,
) -> Archiver<FakeStreams, LegacyResolver, FakeCovers, FakeStore> {
    Archiver::new(streams, LegacyResolver, covers, store, options, fixed_now)
}

// ---------------------------------------------------------------------------
// 1. byte goldens against Go's archive/zip
// ---------------------------------------------------------------------------

#[test]
fn oracle_is_self_consistent() {
    let m = load_manifest();
    assert_eq!(m.fixed_unix, FIXED_UNIX, "oracle fixed timestamp drift");
    assert!(!m.goldens.is_empty(), "oracle has no goldens");
    for g in &m.goldens {
        assert_eq!(g.fixed_unix, FIXED_UNIX);
        assert_eq!(
            unhex(&g.bytes_hex),
            golden_bytes(&g.name),
            "{}: manifest hex vs zip file disagree",
            g.name
        );
        assert_eq!(g.zip_file, format!("{}.zip", g.name));
    }
}

/// Rebuild one golden with the Rust writer: `comment` + entries in order with
/// the same `Modified` semantics Go used.
async fn build_golden(g: &Golden) -> Vec<u8> {
    let mut z = create_zip_writer(Vec::new(), "", 0);
    z.set_comment(g.comment.clone()).unwrap();
    // create_zip_writer sets the raw comment; the oracle's comment comes from
    // its own header, so overwrite it to match exactly.
    for e in g.entries.iter().flatten() {
        let modified = match e.modified.as_str() {
            "fixed" => Some(FIXED_UNIX),
            "now" => Some(NOW_UNIX),
            _ => None,
        };
        let header = ZipFileHeader {
            name: e.name.clone(),
            modified,
            method: ZipMethod::Store,
            non_utf8: false,
        };
        let mut entry = z.create_header(&header).await.unwrap();
        entry.write(&unhex(&e.data_hex)).await.unwrap();
        entry.close().await.unwrap();
    }
    z.close().await.unwrap();
    z.into_inner()
}

#[tokio::test]
async fn deterministic_goldens_are_byte_identical_to_go() {
    let m = load_manifest();
    let mut checked = 0usize;
    for g in m.goldens.iter().filter(|g| g.deterministic) {
        let mine = build_golden(g).await;
        assert_eq!(
            mine,
            golden_bytes(&g.name),
            "{}: Rust bytes differ from Go's archive/zip",
            g.name
        );
        assert_eq!(eocd_comment(&mine), g.comment);
        checked += 1;
    }
    assert!(
        checked >= 4,
        "expected >= 4 deterministic goldens, got {checked}"
    );
}

#[tokio::test]
async fn cover_art_goldens_match_go_up_to_the_now_entry() {
    let m = load_manifest();
    let mut checked = 0usize;
    for g in m.goldens.iter().filter(|g| !g.deterministic) {
        let mine = build_golden(g).await;
        let go = golden_bytes(&g.name);
        let prefix = unhex(&g.deterministic_prefix_hex);
        assert_eq!(
            &mine[..prefix.len()],
            &prefix[..],
            "{}: prefix before the time.Now() entry differs from Go",
            g.name
        );
        assert_eq!(go[..prefix.len()], prefix[..], "oracle prefix vs zip file");
        checked += 1;
    }
    assert!(
        checked >= 3,
        "expected >= 3 now-stamped goldens, got {checked}"
    );
}

#[tokio::test]
async fn every_golden_entry_matches_go_fields() {
    let m = load_manifest();
    for g in &m.goldens {
        let mine = build_golden(g).await;
        let entries = read_zip(&mine);
        let go_entries: &[OracleEntry] = g.entries.as_deref().unwrap_or(&[]);
        assert_eq!(entries.len(), go_entries.len(), "{}: entry count", g.name);
        for (mine_e, go_e) in entries.iter().zip(go_entries) {
            assert_eq!(mine_e.name, go_e.name, "{}: name", g.name);
            assert_eq!(mine_e.method, go_e.method, "{}: method", g.name);
            assert_eq!(
                mine_e.flags, go_e.flags,
                "{}: flags for {:?} (rust {:016b} vs go {:016b})",
                g.name, mine_e.name, mine_e.flags, go_e.flags
            );
            assert_eq!(
                mine_e.crc, go_e.crc32,
                "{}: crc32 for {:?}",
                g.name, mine_e.name
            );
            assert_eq!(mine_e.uncomp_size, go_e.uncomp_size, "{}: size", g.name);
            // Store: the compressed size equals the raw size.
            assert_eq!(go_e.comp_size, go_e.uncomp_size, "{}: comp==uncomp", g.name);
            assert_eq!(
                mine_e.data,
                unhex(&go_e.data_hex),
                "{}: stored bytes",
                g.name
            );
            assert_eq!(
                mine_e.offset as i64, go_e.local_offset,
                "{}: local offset",
                g.name
            );
            // The extended-timestamp extra field carries Unix seconds, so it is
            // reproducible even for Go's time.Now() entry as long as the port's
            // clock returns the same instant.
            if go_e.modified != "now" {
                assert_eq!(
                    mine_e.extra,
                    unhex(&go_e.extra_hex),
                    "{}: extra field for {:?}",
                    g.name,
                    mine_e.name
                );
            } else {
                // Go's time.Now() seconds are not reproducible, so compare the
                // field's shape (id, size, flags) and let the seconds differ.
                assert_eq!(
                    &mine_e.extra[..5],
                    &unhex(&go_e.extra_hex)[..5],
                    "{}: extra-field header for {:?}",
                    g.name,
                    mine_e.name
                );
                assert_eq!(mine_e.extra.len(), 9, "{}: extra length", g.name);
            }
            if go_e.modified != "now" {
                assert_eq!(mine_e.mod_date, go_e.mod_date, "{}: ms-dos date", g.name);
                assert_eq!(mine_e.mod_time, go_e.mod_time, "{}: ms-dos time", g.name);
            } else {
                // Go's time.Now() stamp is the local wall clock; the port uses
                // UTC. The extended timestamp above is the authoritative field.
                assert!(
                    mine_e.mod_date != 0 || mine_e.mod_time != 0,
                    "{}: now entry must still carry an MS-DOS stamp",
                    g.name
                );
            }
        }
    }
}

#[test]
fn ms_dos_table_matches_go() {
    let m = load_manifest();
    for case in &m.msdos_cases {
        assert_eq!(
            ms_dos_time(case.unix),
            (case.date, case.time),
            "ms_dos_time({})",
            case.unix
        );
    }
    assert!(m.msdos_cases.len() >= 8);
}

#[test]
fn crc32_table_matches_go() {
    let m = load_manifest();
    for case in &m.crc32_cases {
        assert_eq!(crc32_ieee(&unhex(&case.input_hex)), case.crc);
    }
    assert!(m.crc32_cases.len() >= 5);
}

#[test]
fn absolute_path_matches_file_path_join() {
    let m = load_manifest();
    for case in &m.join_cases {
        assert_eq!(
            absolute_path(&case.library, &case.path),
            case.joined,
            "filepath.Join({:?}, {:?})",
            case.library,
            case.path
        );
    }
    assert!(m.join_cases.len() >= 10);
    // Clean's own arms, also from the same Go function.
    assert_eq!(path_clean(""), ".");
    assert_eq!(path_clean("."), ".");
    assert_eq!(path_clean("//a//b/"), "/a/b");
    assert_eq!(path_clean("../a/.."), "..");
    assert_eq!(path_clean("/.."), "/");
    assert_eq!(path_clean("/../a"), "/a");
    assert_eq!(path_clean("a/"), "a");
    assert_eq!(path_clean("/a/./b/"), "/a/b");
    assert_eq!(path_clean("../../a/.."), "../..");
}

#[test]
fn cover_extension_table_matches_go_detect_content_type() {
    let m = load_manifest();
    for case in &m.cover_ext_cases {
        let payload = unhex(&case.payload_hex);
        assert_eq!(
            cover_art_extension(&payload),
            case.ext,
            "{}: coverArtExtension",
            case.name
        );
        // The four image arms Go maps, and nothing else, may produce an ext.
        if case.ext.is_empty() {
            assert_ne!(
                case.content_type, "image/jpeg",
                "{}: jpeg must map to jpg",
                case.name
            );
        }
    }
    assert!(m.cover_ext_cases.len() >= 15);
    // Spot-check the sniffed types themselves for the arms Go cares about.
    let cases: BTreeMap<&str, &str> = m
        .cover_ext_cases
        .iter()
        .map(|c| (c.name.as_str(), c.ext.as_str()))
        .collect();
    assert_eq!(cases["jpeg"], "jpg");
    assert_eq!(cases["png"], "png");
    assert_eq!(cases["webp"], "webp");
    assert_eq!(cases["gif89a"], "gif");
    assert_eq!(cases["gif87a"], "gif");
    assert_eq!(cases["icon"], "");
    assert_eq!(cases["octet"], "");
    // Go's sniff window is 512 bytes; a signature inside it is still seen.
    assert_eq!(cases["jpeg past sniff length"], "jpg");
}

#[test]
fn m3u_duration_formatting_matches_go_percent_f() {
    let m = load_manifest();
    for case in &m.m3u_duration_cases {
        let track = ArchiveMediaFile {
            duration: case.duration,
            artist: "Artist".to_string(),
            title: "Title".to_string(),
            path: "x.mp3".to_string(),
            ..ArchiveMediaFile::default()
        };
        let out = to_m3u8(&[track], "P", false);
        assert!(
            out.contains(&case.line),
            "duration {}: expected {:?}, got {:?}",
            case.duration,
            case.line,
            out
        );
    }
    assert!(m.m3u_duration_cases.len() >= 10);
}

#[test]
fn oracle_zip_files_open_and_match_their_names() {
    for name in [
        "album_transcoded",
        "album_raw_zero_time",
        "artist_root_and_albums",
        "playlist_with_m3u",
        "multi_disc",
        "utf8_names",
        "empty",
        "share",
    ] {
        let bytes = golden_bytes(name);
        let entries = read_zip(&bytes);
        assert!(!entries.is_empty() || name == "empty");
        for e in &entries {
            assert_eq!(e.method, 0, "{name}: Go wrote Store for {:?}", e.name);
            assert_eq!(e.crc, crc32_ieee(&e.data), "{name}: crc vs data");
        }
    }
}

// ---------------------------------------------------------------------------
// 2. layout, ported from core/archiver_test.go
// ---------------------------------------------------------------------------

#[tokio::test]
async fn zip_album_names_entries_after_the_resolved_format() {
    // Go: `It("zips an album correctly")`.
    let mfs = vec![
        mf("test_data/01 - track1.mp3", "mp3", "1", "Album/Promo"),
        mf("test_data/02 - track2.mp3", "mp3", "1", "Album/Promo"),
    ];
    let mut a = archiver(
        FakeStore {
            tracks: mfs,
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_album(&mut z, "1", "mp3", 128).await.unwrap();
    let out = z.into_inner();

    let entries = read_zip(&out);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "Album_Promo/01 - track1.mp3");
    assert_eq!(entries[1].name, "Album_Promo/02 - track2.mp3");
    assert_eq!(entries[0].data, b"test");
    assert_eq!(
        eocd_comment(&out),
        "Downloaded from Navidrome, transcoded to mp3 128bps"
    );

    // Go asserts the query it asked for: Eq{"album_id": id}, Sort "album".
    assert_eq!(
        a.store.queries,
        vec![TrackQuery {
            filter: TrackFilter::AlbumId("1".to_string()),
            sort: "album".to_string(),
        }]
    );
    // Requested format/bitrate reach the streamer, like Request{mp3,128}.
    assert_eq!(
        a.streams
            .requests
            .iter()
            .map(|(_, r)| r.clone())
            .collect::<Vec<_>>(),
        vec![
            Request {
                format: "mp3".to_string(),
                bit_rate: 128,
                ..Request::default()
            },
            Request {
                format: "mp3".to_string(),
                bit_rate: 128,
                ..Request::default()
            }
        ]
    );
}

#[tokio::test]
async fn raw_format_streams_the_file_and_keeps_the_suffix() {
    // Go: format "raw" → resolveRequest returns Request{Format:"raw"}, and
    // addFileToZip opens the file itself (core/archiver.go:269-274).
    let dir = std::env::temp_dir().join(format!("nd-archiver-raw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("01 - track1.mp3");
    std::fs::write(&file, b"raw-bytes").unwrap();

    let track = ArchiveMediaFile {
        id: "t1".to_string(),
        library_path: dir.to_str().unwrap().to_string(),
        path: "01 - track1.mp3".to_string(),
        suffix: "mp3".to_string(),
        album_id: "1".to_string(),
        album: "Album".to_string(),
        updated_at: Some(FIXED_UNIX),
        ..ArchiveMediaFile::default()
    };
    let mut a = archiver(
        FakeStore {
            tracks: vec![track],
            ..FakeStore::default()
        },
        FakeStreams::default(),
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "raw", 128);
    a.zip_album(&mut z, "1", "raw", 128).await.unwrap();
    let out = z.into_inner();
    let entries = read_zip(&out);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "Album/01 - track1.mp3");
    assert_eq!(entries[0].data, b"raw-bytes");
    assert_eq!(eocd_comment(&out), DEFAULT_ZIP_COMMENT);
    // No ffmpeg was asked for anything.
    assert!(a.streams.requests.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn resolved_format_names_the_entry_not_the_requested_one() {
    // Go: `fakeDecider` returning {opus,128,48k,2} renames the entry
    // (`It("streams the request resolved by the transcode decider...")`).
    struct OpusResolver;
    impl nd_stream::archiver::RequestResolver for OpusResolver {
        fn resolve(&self, _mf: &ArchiveMediaFile, _format: &str, bit_rate: i64) -> Request {
            Request {
                format: "opus".to_string(),
                bit_rate,
                sample_rate: 48_000,
                channels: 2,
                ..Request::default()
            }
        }
    }
    let track = ArchiveMediaFile {
        path: "test_data/01 - track1.flac".to_string(),
        id: "t1".to_string(),
        suffix: "flac".to_string(),
        album_id: "1".to_string(),
        album: "Album".to_string(),
        updated_at: Some(FIXED_UNIX),
        ..ArchiveMediaFile::default()
    };
    let mut a = Archiver::new(
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        OpusResolver,
        FakeCovers::default(),
        FakeStore {
            tracks: vec![track],
            ..FakeStore::default()
        },
        ArchiverOptions::default(),
        fixed_now,
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_album(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries[0].name, "Album/01 - track1.opus");
    assert_eq!(
        a.streams.requests[0].1,
        Request {
            format: "opus".to_string(),
            bit_rate: 128,
            sample_rate: 48_000,
            channels: 2,
            ..Request::default()
        }
    );
}

#[tokio::test]
async fn zip_artist_uses_the_participation_filter_and_root_cover() {
    // Go: `It("zips an artist's albums correctly")` plus the cover-art context
    // asserting `folder.png` at the root and one cover per album folder.
    let mut t1 = mf("test_data/01 - track1.mp3", "mp3", "1", "Album 1");
    t1.album_image_hash = "hash1".to_string();
    let mut t2 = mf("test_data/02 - track2.mp3", "mp3", "2", "Album 2");
    t2.album_image_hash = "hash2".to_string();
    let mut covers = FakeCovers::default();
    covers.images.insert("al-1_hash1".to_string(), jpeg());
    covers.images.insert("al-2_hash2".to_string(), jpeg());
    covers.images.insert("ar-1".to_string(), png());

    let mut a = archiver(
        FakeStore {
            tracks: vec![t1, t2],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        covers,
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_artist(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());

    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0].name, "Album 1/01 - track1.mp3");
    assert_eq!(entries[1].name, "Album 1/folder.jpg");
    assert_eq!(entries[2].name, "Album 2/02 - track2.mp3");
    assert_eq!(entries[3].name, "Album 2/folder.jpg");
    assert_eq!(entries[4].name, "folder.png");
    assert_eq!(entries[4].data, png());

    assert_eq!(
        a.store.queries,
        vec![TrackQuery {
            filter: TrackFilter::AlbumArtistParticipant {
                artist_id: "1".to_string()
            },
            sort: "album".to_string(),
        }]
    );
    // Every cover read goes through the 500px, non-square path.
    assert_eq!(
        a.covers.requests,
        vec![
            CoverRequest {
                id: "al-1_hash1".to_string(),
                size: ARCHIVE_COVER_ART_SIZE,
                square: false,
                elevated: false
            },
            CoverRequest {
                id: "al-2_hash2".to_string(),
                size: ARCHIVE_COVER_ART_SIZE,
                square: false,
                elevated: false
            },
            CoverRequest {
                id: "ar-1".to_string(),
                size: ARCHIVE_COVER_ART_SIZE,
                square: false,
                elevated: false
            },
        ]
    );
}

#[tokio::test]
async fn multi_disc_tracks_get_a_disc_prefix() {
    let mk = |path: &str, disc: i32| ArchiveMediaFile {
        disc_number: disc,
        ..mf(path, "mp3", "1", "Album")
    };
    let mut a = archiver(
        FakeStore {
            tracks: vec![
                mk("a/01 - one.mp3", 1),
                mk("b/02 - two.mp3", 2),
                mk("c/03 - three.mp3", 2),
            ],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"x".to_vec(),
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "opus", 64);
    a.zip_album(&mut z, "1", "opus", 64).await.unwrap();
    let out = z.into_inner();
    let entries = read_zip(&out);
    assert_eq!(
        entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
        vec![
            "Album/Disc 01/01 - one.opus",
            "Album/Disc 02/02 - two.opus",
            "Album/Disc 02/03 - three.opus",
        ]
    );
    assert_eq!(
        eocd_comment(&out),
        "Downloaded from Navidrome, transcoded to opus 64bps"
    );
}

#[tokio::test]
async fn single_disc_album_has_no_disc_prefix() {
    let mk = |path: &str| ArchiveMediaFile {
        disc_number: 1,
        ..mf(path, "mp3", "1", "Album")
    };
    let mut a = archiver(
        FakeStore {
            tracks: vec![mk("a/01.mp3"), mk("b/02.mp3")],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"x".to_vec(),
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_album(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 2);
    assert!(!entries.iter().any(|e| e.name.contains("Disc ")));
}

/// Run the artist layout over a track set, returning folder/file names — the
/// port of `zipArtistEntries` in `core/archiver_test.go`.
async fn zip_artist_entries(tracks: Vec<ArchiveMediaFile>, append_version: bool) -> Vec<String> {
    let mut a = archiver(
        FakeStore {
            tracks,
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions {
            append_album_version: append_version,
            ..ArchiverOptions::default()
        },
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_artist(&mut z, "1", "mp3", 128).await.unwrap();
    read_zip(&z.into_inner())
        .into_iter()
        .map(|e| e.name)
        .collect()
}

#[tokio::test]
async fn albums_stay_in_query_order() {
    let names = zip_artist_entries(
        vec![
            mf("a/01.mp3", "mp3", "3", "Album C"),
            mf("a/01.mp3", "mp3", "1", "Album A"),
            mf("a/02.mp3", "mp3", "1", "Album A"),
            mf("a/01.mp3", "mp3", "2", "Album B"),
        ],
        true,
    )
    .await;
    assert_eq!(
        names,
        [
            "Album C/01.mp3",
            "Album A/01.mp3",
            "Album A/02.mp3",
            "Album B/01.mp3",
        ]
    );
}

#[tokio::test]
async fn year_suffix_when_it_tells_albums_apart() {
    let mut a1 = mf("a/01 - Intro.mp3", "mp3", "1", "Greatest Hits");
    a1.year = 2001;
    let mut a2 = mf("b/01 - Intro.mp3", "mp3", "2", "Greatest Hits");
    a2.year = 2005;
    assert_eq!(
        zip_artist_entries(vec![a1, a2], true).await,
        [
            "Greatest Hits [2001]/01 - Intro.mp3",
            "Greatest Hits [2005]/01 - Intro.mp3"
        ]
    );
}

#[tokio::test]
async fn release_year_wins_over_year() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "Greatest Hits");
    a1.year = 1996;
    a1.release_year = 2001;
    let mut a2 = mf("b/01.mp3", "mp3", "2", "Greatest Hits");
    a2.year = 1996;
    a2.release_year = 2011;
    let mut a3 = mf("c/01.mp3", "mp3", "3", "Greatest Hits");
    a3.year = 1996;
    assert_eq!(
        zip_artist_entries(vec![a1, a2, a3], true).await,
        [
            "Greatest Hits [2001]/01.mp3",
            "Greatest Hits [2011]/01.mp3",
            "Greatest Hits [1996]/01.mp3",
        ]
    );
}

#[tokio::test]
async fn folder_name_follows_the_full_album_name() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "Greatest Hits");
    a1.year = 2001;
    a1.tags
        .insert("albumversion".to_string(), vec!["Original".to_string()]);
    let mut a2 = mf("b/01.mp3", "mp3", "2", "Greatest Hits");
    a2.year = 2005;
    a2.tags
        .insert("albumversion".to_string(), vec!["CD/Digital".to_string()]);
    assert_eq!(
        zip_artist_entries(vec![a1, a2], true).await,
        [
            "Greatest Hits (Original)/01.mp3",
            "Greatest Hits (CD_Digital)/01.mp3"
        ]
    );
}

#[tokio::test]
async fn album_version_wins_when_not_part_of_the_name() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "Greatest Hits");
    a1.year = 2001;
    a1.tags
        .insert("albumversion".to_string(), vec!["Original".to_string()]);
    let mut a2 = mf("b/01.mp3", "mp3", "2", "Greatest Hits");
    a2.year = 2005;
    a2.tags.insert(
        "albumversion".to_string(),
        vec!["Deluxe Edition".to_string()],
    );
    assert_eq!(
        zip_artist_entries(vec![a1, a2], false).await,
        [
            "Greatest Hits [Original]/01.mp3",
            "Greatest Hits [Deluxe Edition]/01.mp3"
        ]
    );
}

#[tokio::test]
async fn the_album_without_the_field_stays_unsuffixed() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "Greatest Hits");
    a1.year = 2001;
    let mut a2 = mf("b/01.mp3", "mp3", "2", "Greatest Hits");
    a2.year = 2005;
    a2.tags.insert(
        "albumversion".to_string(),
        vec!["Deluxe Edition".to_string()],
    );
    assert_eq!(
        zip_artist_entries(vec![a1, a2], false).await,
        [
            "Greatest Hits/01.mp3",
            "Greatest Hits [Deluxe Edition]/01.mp3"
        ]
    );
}

#[tokio::test]
async fn a_field_empty_on_several_albums_is_skipped() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "Greatest Hits");
    a1.year = 2001;
    let mut a2 = mf("b/01.mp3", "mp3", "2", "Greatest Hits");
    a2.year = 2005;
    let mut a3 = mf("c/01.mp3", "mp3", "3", "Greatest Hits");
    a3.year = 2010;
    a3.tags.insert(
        "albumversion".to_string(),
        vec!["Deluxe Edition".to_string()],
    );
    assert_eq!(
        zip_artist_entries(vec![a1, a2, a3], false).await,
        [
            "Greatest Hits [2001]/01.mp3",
            "Greatest Hits [2005]/01.mp3",
            "Greatest Hits [2010]/01.mp3",
        ]
    );
}

#[tokio::test]
async fn a_field_identical_on_every_album_is_skipped() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "Live");
    a1.year = 2001;
    a1.mbz_album_type = "album".to_string();
    a1.catalog_num = "CAT-1".to_string();
    let mut a2 = mf("b/01.mp3", "mp3", "2", "Live");
    a2.year = 2001;
    a2.mbz_album_type = "album".to_string();
    a2.catalog_num = "CAT-2".to_string();
    assert_eq!(
        zip_artist_entries(vec![a1, a2], true).await,
        ["Live [CAT-1]/01.mp3", "Live [CAT-2]/01.mp3"]
    );
}

#[tokio::test]
async fn falls_back_to_the_album_id_prefix() {
    let mut a1 = mf("a/01.mp3", "mp3", "0123456789abcdef", "Greatest Hits");
    a1.year = 2001;
    let mut a2 = mf("b/01.mp3", "mp3", "fedcba9876543210", "Greatest Hits");
    a2.year = 2001;
    assert_eq!(
        zip_artist_entries(vec![a1, a2], true).await,
        [
            "Greatest Hits [012345]/01.mp3",
            "Greatest Hits [fedcba]/01.mp3"
        ]
    );
}

#[tokio::test]
async fn names_that_sanitize_to_the_same_folder_clash() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "A/B");
    a1.year = 2001;
    let mut a2 = mf("b/01.mp3", "mp3", "2", r"A\B");
    a2.year = 2005;
    assert_eq!(
        zip_artist_entries(vec![a1, a2], true).await,
        ["A_B [2001]/01.mp3", "A_B [2005]/01.mp3"]
    );
}

#[tokio::test]
async fn the_suffix_itself_is_sanitized() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "Hits");
    a1.tags
        .insert("albumversion".to_string(), vec!["Vinyl".to_string()]);
    let mut a2 = mf("b/01.mp3", "mp3", "2", "Hits");
    a2.tags
        .insert("albumversion".to_string(), vec!["CD/Digital".to_string()]);
    assert_eq!(
        zip_artist_entries(vec![a1, a2], false).await,
        ["Hits [Vinyl]/01.mp3", "Hits [CD_Digital]/01.mp3"]
    );
}

#[tokio::test]
async fn a_unique_name_is_left_alone() {
    let mut a1 = mf("a/01.mp3", "mp3", "1", "Greatest Hits");
    a1.year = 2001;
    let mut a2 = mf("b/01.mp3", "mp3", "2", "Other");
    a2.year = 2005;
    assert_eq!(
        zip_artist_entries(vec![a1, a2], true).await,
        ["Greatest Hits/01.mp3", "Other/01.mp3"]
    );
}

#[tokio::test]
async fn disambiguator_order_and_album_folder_helpers() {
    // The disambiguator table itself, in Go's order (core/archiver.go:112-130).
    let mut track = mf("a/01.mp3", "mp3", "0123456789abcdef", "Album");
    track
        .tags
        .insert("albumversion".to_string(), vec!["V".to_string()]);
    track.release_year = 2011;
    track.year = 1996;
    track.mbz_album_type = "album".to_string();
    track
        .tags
        .insert("recordlabel".to_string(), vec!["LM".to_string()]);
    track.catalog_num = "CAT".to_string();
    assert_eq!(
        album_disambiguators(&track),
        [
            "V",
            "2011",
            "album",
            "LM",
            "CAT",
            "012345",
            "0123456789abcdef"
        ]
    );

    // Grouping and disc counting (utils/slice.Group + GroupOrdered).
    let t1 = mf("a/1.mp3", "mp3", "1", "A");
    let t2 = mf("b/2.mp3", "mp3", "2", "B");
    let t3 = mf("c/3.mp3", "mp3", "1", "A");
    let tracks = vec![t1, t2, t3];
    let groups = nd_stream::archiver::group_ordered(&tracks, |t| t.album_id.clone());
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].len(), 2);
    assert_eq!(album_count_discs(&groups[0]), 1);

    // album_folders over the grouped albums.
    let folders = album_folders(&groups, true);
    assert_eq!(folders.get("1").map(String::as_str), Some("A"));
    assert_eq!(folders.get("2").map(String::as_str), Some("B"));
    assert_eq!(append_suffix("X", "Live"), "X (Live)");
}

#[tokio::test]
async fn zip_share_numbers_entries_and_is_not_a_playlist() {
    // Go: `It("zips a share correctly")` — 2 entries, no M3U.
    let mut t1 = mf("test_data/01 - track1.mp3", "mp3", "1", "Album");
    t1.id = "1".to_string();
    t1.artist = "Artist 1".to_string();
    t1.title = "track1".to_string();
    let mut t2 = mf("test_data/02 - track2.mp3", "mp3", "2", "Album");
    t2.id = "2".to_string();
    t2.artist = "Artist 2".to_string();
    t2.title = "track2".to_string();
    let share = ArchiveShare {
        id: "1".to_string(),
        downloadable: true,
        format: "mp3".to_string(),
        max_bit_rate: 128,
        tracks: vec![t1, t2],
        ..ArchiveShare::default()
    };
    let mut a = archiver(
        FakeStore::default(),
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), &share.format, share.max_bit_rate);
    a.zip_share(&mut z, &share, 0).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "01 - Artist 1 - track1.mp3");
    assert_eq!(entries[1].name, "02 - Artist 2 - track2.mp3");
}

#[tokio::test]
async fn zip_share_refuses_a_non_downloadable_share() {
    let share = ArchiveShare {
        id: "1".to_string(),
        downloadable: false,
        ..ArchiveShare::default()
    };
    let mut a = archiver(
        FakeStore::default(),
        FakeStreams::default(),
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    assert_eq!(
        a.zip_share(&mut z, &share, 0).await.err(),
        Some(ArchiverError::NotAuthorized)
    );
}

#[tokio::test]
async fn zip_share_elevates_only_the_cover_read() {
    // Go: `request.WithUser(ctx, model.User{IsAdmin: true})` for the cover only,
    // and `coverRequest{admin: true}` in the test's mock.
    let mut covers = FakeCovers::default();
    covers.images.insert("pl-10".to_string(), jpeg());
    let mut t1 = mf("test_data/01 - track1.mp3", "mp3", "1", "Album");
    t1.id = "1".to_string();
    t1.artist = "Artist 1".to_string();
    t1.title = "track1".to_string();
    let share = ArchiveShare {
        id: "1".to_string(),
        downloadable: true,
        format: "mp3".to_string(),
        max_bit_rate: 128,
        resource_type: "playlist".to_string(),
        resource_ids: "10".to_string(),
        tracks: vec![t1],
    };
    let mut a = archiver(
        FakeStore::default(),
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        covers,
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_share(&mut z, &share, 0).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].name, "folder.jpg");
    assert_eq!(
        a.covers.requests,
        vec![CoverRequest {
            id: "pl-10".to_string(),
            size: ARCHIVE_COVER_ART_SIZE,
            square: false,
            elevated: true,
        }]
    );
    // The share id is the M3U title only for playlists; shares emit no M3U.
    assert!(!entries.iter().any(|e| e.name.ends_with(".m3u")));
}

#[tokio::test]
async fn zip_share_cover_id_follows_the_resource_type() {
    let mk = |resource_type: &str, ids: &str| ArchiveShare {
        id: "1".to_string(),
        resource_type: resource_type.to_string(),
        resource_ids: ids.to_string(),
        tracks: vec![ArchiveMediaFile {
            album_id: "9".to_string(),
            ..ArchiveMediaFile::default()
        }],
        ..ArchiveShare::default()
    };
    let opts = nd_stream::archiver::ArchiverOptions::default();
    assert_eq!(mk("album", "7,8").cover_art_id(0, &opts).render(), "al-7");
    assert_eq!(
        mk("playlist", "10").cover_art_id(0, &opts).render(),
        "pl-10"
    );
    assert_eq!(mk("artist", "3").cover_art_id(0, &opts).render(), "ar-3");
    // Track resource: Go picks a random track; the port takes the index.
    let mut s = mk("track", "");
    s.tracks.push(ArchiveMediaFile {
        album_id: "10".to_string(),
        ..ArchiveMediaFile::default()
    });
    assert_eq!(s.cover_art_id(0, &opts).render(), "al-9");
    assert_eq!(s.cover_art_id(1, &opts).render(), "al-10");
    let empty = ArchiveShare {
        id: "1".to_string(),
        resource_type: "track".to_string(),
        ..ArchiveShare::default()
    };
    assert_eq!(empty.cover_art_id(0, &opts).render(), "");
}

#[tokio::test]
async fn share_cover_follows_the_media_file_cover_chain() {
    // `model.MediaFile.CoverArtID` (`model/mediafile.go:137-144`): the track's
    // own artwork first, then the disc, then the album.
    let opts = nd_stream::archiver::ArchiverOptions::default();
    let mut t = ArchiveMediaFile {
        id: "mf-1".to_string(),
        album_id: "5".to_string(),
        disc_number: 2,
        has_cover_art: true,
        image_hash: "hsh".to_string(),
        ..ArchiveMediaFile::default()
    };
    let share = ArchiveShare {
        id: "1".to_string(),
        resource_type: "track".to_string(),
        tracks: vec![t.clone()],
        ..ArchiveShare::default()
    };
    assert_eq!(share.cover_art_id(0, &opts).render(), "mf-mf-1_hsh");
    assert_eq!(t.cover_art_id(&opts).render(), "mf-mf-1_hsh");

    // Feature off → the disc fallback, whose id is `album:disc`.
    let off = nd_stream::archiver::ArchiverOptions {
        enable_media_file_cover_art: false,
        ..nd_stream::archiver::ArchiverOptions::default()
    };
    assert_eq!(t.cover_art_id(&off).render(), "dc-5:2_hsh");

    // Disc 0 → the album.
    t.disc_number = 0;
    assert_eq!(t.cover_art_id(&off).render(), "al-5");
    assert_eq!(t.disc_cover_art_id().render(), "al-5");
    assert_eq!(t.album_cover_art_id().render(), "al-5");
}

#[tokio::test]
async fn zip_playlist_writes_entries_cover_and_m3u() {
    // Go: `It("zips a playlist correctly")`, M3U content compared verbatim.
    let mut t1 = mf("test_data/01 - track1.mp3", "mp3", "1", "Album 1");
    t1.id = "t1".to_string();
    t1.artist = "AC/DC".to_string();
    t1.title = "track1".to_string();
    t1.duration = 0.0;
    let mut t2 = mf("test_data/02 - track2.mp3", "mp3", "1", "Album 1");
    t2.id = "t2".to_string();
    t2.artist = "Artist 2".to_string();
    t2.title = "track2".to_string();
    t2.duration = 0.0;
    let store = FakeStore {
        playlist: Some(ArchivePlaylist {
            id: "1".to_string(),
            name: "Test Playlist".to_string(),
            tracks: vec![t1, t2],
            ..ArchivePlaylist::default()
        }),
        ..FakeStore::default()
    };
    let mut a = archiver(
        store,
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_playlist(&mut z, "1", "mp3", 128).await.unwrap();
    let out = z.into_inner();
    let entries = read_zip(&out);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name, "01 - AC_DC - track1.mp3");
    assert_eq!(entries[1].name, "02 - Artist 2 - track2.mp3");
    assert_eq!(entries[2].name, "Test Playlist.m3u");
    assert_eq!(
        String::from_utf8(entries[2].data.clone()).unwrap(),
        "#EXTM3U\n#PLAYLIST:Test Playlist\n#EXTINF:0,AC/DC - track1\n01 - AC_DC - track1.mp3\n#EXTINF:0,Artist 2 - track2\n02 - Artist 2 - track2.mp3\n"
    );
    // Go calls GetWithTracks(ctx, id, true, false).
    assert_eq!(a.store.playlist_calls, vec![("1".to_string(), true, false)]);
}

#[tokio::test]
async fn playlist_m3u_names_lines_after_the_resolved_format() {
    struct OpusResolver;
    impl nd_stream::archiver::RequestResolver for OpusResolver {
        fn resolve(&self, _mf: &ArchiveMediaFile, _format: &str, bit_rate: i64) -> Request {
            Request {
                format: "opus".to_string(),
                bit_rate,
                ..Request::default()
            }
        }
    }
    let mut t1 = mf("test_data/01 - track1.flac", "flac", "1", "Album");
    t1.artist = "Artist 1".to_string();
    t1.title = "track1".to_string();
    let store = FakeStore {
        playlist: Some(ArchivePlaylist {
            id: "1".to_string(),
            name: "Test Playlist".to_string(),
            tracks: vec![t1],
            ..ArchivePlaylist::default()
        }),
        ..FakeStore::default()
    };
    let mut a = Archiver::new(
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        OpusResolver,
        FakeCovers::default(),
        store,
        ArchiverOptions::default(),
        fixed_now,
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_playlist(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries[0].name, "01 - Artist 1 - track1.opus");
    let m3u = String::from_utf8(entries.last().unwrap().data.clone()).unwrap();
    assert!(m3u.contains("01 - Artist 1 - track1.opus"), "{m3u}");
}

#[tokio::test]
async fn playlist_cover_lands_at_the_root() {
    let mut covers = FakeCovers::default();
    covers.images.insert("pl-1".to_string(), jpeg());
    let mut t1 = mf("test_data/01 - track1.mp3", "mp3", "1", "Album");
    t1.artist = "Artist 1".to_string();
    t1.title = "track1".to_string();
    let store = FakeStore {
        playlist: Some(ArchivePlaylist {
            id: "1".to_string(),
            name: "Test Playlist".to_string(),
            tracks: vec![t1],
            ..ArchivePlaylist::default()
        }),
        ..FakeStore::default()
    };
    let mut a = archiver(
        store,
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        covers,
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_playlist(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1].name, "folder.jpg");
    assert_eq!(entries[2].name, "Test Playlist.m3u");
    assert_eq!(a.covers.requests[0].id, "pl-1");
    assert!(!a.covers.requests[0].elevated);
}

#[tokio::test]
async fn zip_album_puts_the_cover_in_the_album_folder() {
    // Go: `It("adds the album cover to the album folder")` — one track, one
    // cover, and exactly one artwork read at 500px, non-square.
    let mut covers = FakeCovers::default();
    covers.images.insert("al-1".to_string(), jpeg());
    let mut t1 = mf("test_data/01 - track1.mp3", "mp3", "1", "Album/Promo");
    t1.album_image_hash = String::new();
    let mut a = archiver(
        FakeStore {
            tracks: vec![t1],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        covers,
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_album(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "Album_Promo/01 - track1.mp3");
    assert_eq!(entries[1].name, "Album_Promo/folder.jpg");
    assert_eq!(entries[1].data, jpeg());
    assert_eq!(
        a.covers.requests,
        vec![CoverRequest {
            id: "al-1".to_string(),
            size: ARCHIVE_COVER_ART_SIZE,
            square: false,
            elevated: false,
        }]
    );
}

#[tokio::test]
async fn an_absent_cover_is_skipped_without_an_entry() {
    // Go: `errors.Is(err, artwork.ErrUnavailable)` → log and return, no entry.
    let mut a = archiver(
        FakeStore {
            tracks: vec![mf("a/01.mp3", "mp3", "1", "Album")],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"t".to_vec(),
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_album(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 1);
    assert_eq!(
        a.covers.requests.len(),
        1,
        "Go still asks, then drops the error"
    );
}

#[tokio::test]
async fn an_unreadable_cover_still_builds_the_archive() {
    // Go: `It("still builds the archive when the cover cannot be read")`.
    let covers = FakeCovers {
        error: Some(CoverArtError::Other("boom".to_string())),
        ..FakeCovers::default()
    };
    let mut a = archiver(
        FakeStore {
            tracks: vec![mf("test_data/01 - track1.mp3", "mp3", "1", "Album")],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"test".to_vec(),
            ..FakeStreams::default()
        },
        covers,
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_album(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "Album/01 - track1.mp3");
    assert_eq!(entries[0].data, b"test");
}

#[tokio::test]
async fn an_unrecognized_cover_type_is_not_added() {
    let mut covers = FakeCovers::default();
    // A PDF: sniffed, but Go adds nothing for it (coverArtExtension → "").
    covers
        .images
        .insert("al-1".to_string(), b"%PDF-1.7\n".to_vec());
    let mut t1 = mf("a/01.mp3", "mp3", "1", "Album");
    t1.album_image_hash = "h".to_string();
    let mut a = archiver(
        FakeStore {
            tracks: vec![t1],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"t".to_vec(),
            ..FakeStreams::default()
        },
        covers,
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_album(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 1);
    assert_eq!(a.covers.requests.len(), 1);
}

#[tokio::test]
async fn each_same_named_album_keeps_its_own_cover() {
    let mut covers = FakeCovers::default();
    covers.images.insert("al-1".to_string(), jpeg());
    covers.images.insert("al-2".to_string(), png());
    let mut a1 = mf("test_data/01 - track1.mp3", "mp3", "1", "Greatest Hits");
    a1.year = 2001;
    a1.disc_number = 1;
    let mut a2 = mf("test_data/02 - track2.mp3", "mp3", "2", "Greatest Hits");
    a2.year = 2005;
    a2.disc_number = 1;
    let mut a = archiver(
        FakeStore {
            tracks: vec![a1, a2],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"t".to_vec(),
            ..FakeStreams::default()
        },
        covers,
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_artist(&mut z, "1", "mp3", 128).await.unwrap();
    let entries = read_zip(&z.into_inner());
    let by_name: BTreeMap<String, Vec<u8>> = entries
        .iter()
        .map(|e| (e.name.clone(), e.data.clone()))
        .collect();
    assert_eq!(
        by_name.get("Greatest Hits [2001]/folder.jpg"),
        Some(&jpeg())
    );
    assert_eq!(by_name.get("Greatest Hits [2005]/folder.png"), Some(&png()));
}

#[tokio::test]
async fn album_cover_id_uses_the_album_hash() {
    // Go's AlbumCoverArtID uses AlbumImage, not the track's ItemImage.
    let mut t = mf("a/01.mp3", "mp3", "42", "Album");
    t.album_image_hash = "deadbeefcafe1234".to_string();
    assert_eq!(t.album_cover_art_id().render(), "al-42_deadbeefcafe1234");
    assert_eq!(album_artwork_id("42", "").render(), "al-42");
    assert_eq!(ArtworkId::none().render(), "");
}

// ---------------------------------------------------------------------------
// 3. limiter abort semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn album_archive_aborts_on_the_first_refusal_and_keeps_written_tracks() {
    // Go: `When the transcode limiter rejects a file` — NewStream is called
    // exactly once and the error surfaces (core/archiver.go:80-88).
    let mut a = archiver(
        FakeStore {
            tracks: vec![
                mf("test_data/01 - track1.mp3", "mp3", "1", "Album"),
                mf("test_data/02 - track2.mp3", "mp3", "1", "Album"),
            ],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"test".to_vec(),
            refuse_first: 1,
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    let err = a.zip_album(&mut z, "1", "mp3", 128).await.unwrap_err();
    assert!(err.is_too_many_transcodes(), "{err:?}");
    assert_eq!(a.streams.requests.len(), 1, "loop must bail on the refusal");
    // The writer was closed: whatever was written is a valid archive, and the
    // refused track is absent (its header is written only after the open).
    let out = z.into_inner();
    assert!(read_zip(&out).is_empty());
}

#[tokio::test]
async fn album_archive_keeps_iterating_after_a_non_limiter_failure() {
    // Go only aborts on ErrTooManyTranscodes; a missing file is logged and the
    // loop continues (core/archiver.go:89-91, "not hardened" divergence).
    let mut a = archiver(
        FakeStore {
            tracks: vec![
                mf("test_data/01 - track1.mp3", "mp3", "1", "Album"),
                mf("test_data/02 - track2.mp3", "mp3", "1", "Album"),
            ],
            ..FakeStore::default()
        },
        FakeStreams {
            payload: b"test".to_vec(),
            fail_first: 1,
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    a.zip_album(&mut z, "1", "mp3", 128).await.unwrap();
    assert_eq!(a.streams.requests.len(), 2);
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "Album/02 - track2.mp3");
}

#[tokio::test]
async fn playlist_archive_aborts_and_reports_the_limiter() {
    let store = FakeStore {
        playlist: Some(ArchivePlaylist {
            id: "1".to_string(),
            name: "P".to_string(),
            tracks: vec![
                mf("a/01.mp3", "mp3", "1", "A"),
                mf("b/02.mp3", "mp3", "1", "A"),
            ],
            ..ArchivePlaylist::default()
        }),
        ..FakeStore::default()
    };
    let mut a = archiver(
        store,
        FakeStreams {
            payload: b"t".to_vec(),
            refuse_first: 1,
            ..FakeStreams::default()
        },
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    let err = a.zip_playlist(&mut z, "1", "mp3", 128).await.unwrap_err();
    assert_eq!(err, ArchiverError::Source(SourceError::TooManyTranscodes));
    assert_eq!(a.streams.requests.len(), 1);
    // No M3U was emitted: the archive aborted before the cover and M3U.
    assert!(read_zip(&z.into_inner()).is_empty());
}

#[tokio::test]
async fn repository_errors_propagate() {
    let mut a = archiver(
        FakeStore {
            error: Some("db down".to_string()),
            ..FakeStore::default()
        },
        FakeStreams::default(),
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    let err = a.zip_album(&mut z, "1", "mp3", 128).await.unwrap_err();
    assert!(matches!(err, ArchiverError::Repo(_)));
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    let err = a
        .zip_playlist(&mut z, "missing", "mp3", 128)
        .await
        .unwrap_err();
    assert!(matches!(err, ArchiverError::Repo(_)));
}

#[tokio::test]
async fn a_missing_file_on_the_raw_arm_continues_like_go() {
    let track = ArchiveMediaFile {
        library_path: "/nonexistent-library-for-this-test".to_string(),
        path: "Album/01.mp3".to_string(),
        suffix: "mp3".to_string(),
        album_id: "1".to_string(),
        album: "Album".to_string(),
        ..ArchiveMediaFile::default()
    };
    let mut a = archiver(
        FakeStore {
            tracks: vec![track],
            ..FakeStore::default()
        },
        FakeStreams::default(),
        FakeCovers::default(),
        ArchiverOptions::default(),
    );
    let mut z = create_zip_writer(Vec::new(), "raw", 128);
    // The failure is logged by Go and does not fail the archive.
    a.zip_album(&mut z, "1", "raw", 128).await.unwrap();
    assert!(read_zip(&z.into_inner()).is_empty());
}

// ---------------------------------------------------------------------------
// 4. writer edges (Go's archive/zip error arms)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_comment_is_untouched_when_set_comment_refuses_it() {
    // Go ignores `SetComment`'s error (`_ = z.SetComment(comment)`), so an
    // over-long comment leaves whatever was set before — here, nothing.
    let mut z = create_zip_writer(Vec::new(), "mp3", 128);
    assert_eq!(
        z.comment(),
        "Downloaded from Navidrome, transcoded to mp3 128bps"
    );
    assert!(z.set_comment("x".repeat(65_536)).is_err());
    assert_eq!(
        z.comment(),
        "Downloaded from Navidrome, transcoded to mp3 128bps"
    );
}

#[test]
fn zip_error_messages_match_go_verbatim() {
    // Go's strings (archive/zip/reader.go:30, writer.go:75, 92, 631, 643, 673),
    // compared exactly — the message is part of the observable contract.
    assert_eq!(
        ZipError::UnsupportedMethod.to_string(),
        "zip: unsupported compression algorithm"
    );
    assert_eq!(
        ZipError::CommentTooLong.to_string(),
        "zip: Writer.Comment too long"
    );
    assert_eq!(
        ZipError::WriteToClosedFile.to_string(),
        "zip: write to closed file"
    );
    assert_eq!(
        ZipError::FileClosedTwice.to_string(),
        "zip: file closed twice"
    );
    assert_eq!(
        ZipError::WriteToDirectory.to_string(),
        "zip: write to directory"
    );
    assert_eq!(
        ZipError::ClosedTwice.to_string(),
        "zip: writer closed twice"
    );
}

#[tokio::test]
async fn deflate_is_refused_like_zip_err_algorithm() {
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
async fn an_over_long_comment_is_rejected_and_left_unset() {
    let mut z = ZipWriter::new(Vec::new());
    let long = "x".repeat(65_536);
    assert_eq!(
        z.set_comment(long.clone()).err(),
        Some(ZipError::CommentTooLong)
    );
    assert_eq!(z.comment(), "");
    z.set_comment("ok").unwrap();
    assert_eq!(z.comment(), "ok");
}

#[tokio::test]
async fn an_empty_archive_still_carries_the_comment() {
    let mut z = create_zip_writer(Vec::new(), "mp3", 320);
    z.close().await.unwrap();
    let out = z.into_inner();
    assert_eq!(out, golden_bytes("empty"));
    assert_eq!(
        eocd_comment(&out),
        "Downloaded from Navidrome, transcoded to mp3 320bps"
    );
    assert!(read_zip(&out).is_empty());
}

#[tokio::test]
async fn utf8_flag_follows_go_for_cjk_tilde_and_backslash() {
    let mut z = create_zip_writer(Vec::new(), "raw", 0);
    for name in ["Album 東京/01.mp3", "Hits~/02.mp3", "Plain/03.mp3"] {
        let header = ZipFileHeader::stored(name);

        let mut e = z.create_header(&header).await.unwrap();
        e.write(b"x").await.unwrap();
        e.close().await.unwrap();
    }
    z.close().await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries[0].flags & 0x800, 0x800);
    assert_eq!(entries[1].flags & 0x800, 0x800);
    assert_eq!(entries[2].flags & 0x800, 0);
    // All three carry the data-descriptor flag, as Go always writes it.
    for e in &entries {
        assert_eq!(e.flags & 0x8, 0x8);
    }
}

#[tokio::test]
async fn non_utf8_header_clears_the_flag() {
    let mut z = ZipWriter::new(Vec::new());
    let header = ZipFileHeader {
        name: "Album 東京/01.mp3".to_string(),
        modified: None,
        method: ZipMethod::Store,
        non_utf8: true,
    };
    let mut e = z.create_header(&header).await.unwrap();
    e.write(b"x").await.unwrap();
    e.close().await.unwrap();
    z.close().await.unwrap();
    assert_eq!(read_zip(&z.into_inner())[0].flags & 0x800, 0);
}

#[tokio::test]
async fn zero_timestamp_writes_no_extra_field() {
    let mut z = ZipWriter::new(Vec::new());
    let header = ZipFileHeader::stored("a.mp3");

    let mut e = z.create_header(&header).await.unwrap();
    e.write(b"AAAA").await.unwrap();
    e.close().await.unwrap();
    z.close().await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert!(entries[0].extra.is_empty());
    assert_eq!(entries[0].mod_date, 0);
    assert_eq!(entries[0].mod_time, 0);
}

#[tokio::test]
async fn timestamps_write_the_extended_timestamp_extra() {
    let mut z = ZipWriter::new(Vec::new());
    let header = ZipFileHeader {
        name: "a.mp3".to_string(),
        modified: Some(FIXED_UNIX),
        method: ZipMethod::Store,
        non_utf8: false,
    };
    let mut e = z.create_header(&header).await.unwrap();
    e.write(b"AAAA").await.unwrap();
    e.close().await.unwrap();
    z.close().await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries[0].extra, unhex("5554050001f0d9ee60"));
    assert_eq!(
        (entries[0].mod_date, entries[0].mod_time),
        ms_dos_time(FIXED_UNIX)
    );
}

#[tokio::test]
async fn directory_entries_accept_no_data() {
    let mut z = ZipWriter::new(Vec::new());
    let header = ZipFileHeader::stored("Album/");

    let mut e = z.create_header(&header).await.unwrap();
    assert_eq!(e.write(b"").await, Ok(()));
    assert_eq!(e.write(b"x").await.err(), Some(ZipError::WriteToDirectory));
    z.close().await.unwrap();
    let entries = read_zip(&z.into_inner());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].uncomp_size, 0);
    assert_eq!(entries[0].flags & 0x8, 0, "directories carry no descriptor");
}

/// A sink that fails mid-write, to reach the writer's I/O arms.
struct FailingSink {
    after: usize,
    written: usize,
}

impl AsyncSink for FailingSink {
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> BoxFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.written += data.len();
            if self.written > self.after {
                return Err(std::io::Error::other("sink full"));
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn sink_errors_surface_as_zip_io_errors() {
    // The sink is buffered (Go's bufio.Writer, 4096 bytes), so small writes
    // never reach it: a sink that fails on the first delivery surfaces at the
    // final flush inside `close`, not at `create_header` or `write`.
    let mut z = ZipWriter::new(FailingSink {
        after: 0,
        written: 0,
    });
    let header = ZipFileHeader::stored("a.mp3");
    let mut e = z.create_header(&header).await.unwrap();
    assert_eq!(e.write(b"data").await, Ok(()));
    let err = z.close().await.err();
    assert!(matches!(err, Some(ZipError::Io(_))), "{err:?}");

    // A write larger than the buffer bypasses it (Go's bufio does the same), so
    // a sink that fails partway through surfaces at `write`.
    let mut z = ZipWriter::new(FailingSink {
        after: 5_000,
        written: 0,
    });
    let header = ZipFileHeader::stored("a.mp3");
    let mut e = z.create_header(&header).await.unwrap();
    assert_eq!(e.write(&[7u8; 1_000]).await, Ok(()));
    let err = e.write(&[7u8; 6_000]).await.err();
    assert!(matches!(err, Some(ZipError::Io(_))), "{err:?}");
}
