# nd-tagreader: real-file backend (#08)

The default `taglib` extractor now opens **media files**, using exactly
`lofty = "=0.25.5"`. `lofty` is also a registry alias. `read_metadata(path)`
returns `Result<Info, lofty::error::FileParseError>`; batch `Extractor::parse`
retains the Go skip-on-file-error behavior. Its version is `lofty/0.25.5`, not
an invented version of the Go TagLib fork.

## Backend decision and dependencies

Initially none of lofty/id3/mp4ameta/metaflac/audiotags/taglib/taglib-sys was
available in the local Cargo cache/index (`cargo info --offline` returned 101
for each), and `pkg-config taglib` failed. Work stopped until the parent
explicitly approved network resolution. Cargo then resolved Lofty successfully.

Lofty is pure Rust, MIT OR Apache-2.0, supports the four required container
families, and needs no C library, C compiler, Go/WASM runtime or runtime ffmpeg.
Its default ID3v2 decompression feature is retained, using the Rust flate2
backend. MSRV is 1.89 (Lofty's declaration).

The lock delta is **12 added packages**: lofty plus **11 newly locked transitives**:
adler2 2.0.1, byteorder 1.5.0, crc32fast 1.5.5, data-encoding 2.11.1,
flate2 1.1.11, lofty_attr 0.13.0, miniz_oxide 0.9.1, ogg_pager 0.8.0,
paste 1.0.15, simd-adler32 0.3.10, zlib-rs 0.6.8. No existing package was
upgraded. zlib-rs is locked but not in the active default dependency tree.
`cargo tree --locked -p lofty` has **16 distinct active transitive packages**
(**17 including lofty**), reusing already-locked cfg-if, log, proc-macro2, quote,
syn and unicode-ident. Including dormant zlib-rs, the locked transitive count is 17.
All added packages have permissive licenses; no additional direct/dev dependency
was added.

Supported containers: AAC/ADTS, AIFF, APE, FLAC, MPEG audio, MP4 (including
AAC/ALAC), Musepack, Opus, Ogg Vorbis, Speex, WAV and WavPack.
**Lofty 0.25.5 does not support ASF/WMA.**

## Contract and Go truths

- Output is the tag adapter's `Info` boundary: tags, audio properties and picture
  presence (`model/metadata/metadata.go:21-40`,
  `adapters/gotaglib/gotaglib.go:82-122`). Audio properties include duration in
  seconds rounded to 10ms, bitrate in kbps, channels, sample rate, bit depth and
  codec. Missing optional numeric properties are 0, never fabricated.
- Go's separate `FileInfo` field is filled later by storage
  (`core/storage/local/local.go:90-109`). That existing Rust DTO omission remains
  a #17 integration task, not a tag-reader field that Lofty supplies.
- Batch errors skip the failed file, not the batch (`gotaglib.go:34-43`). Batch
  names must be relative fs.FS-style paths; absolute/parent-traversal names are
  rejected. The direct `read_metadata` API intentionally accepts OS paths.
- Keys are lowercase and multiple values retain their order
  (`gotaglib.go:96-101`). Native Vorbis comments preserve separate aliases rather
  than coalescing `TOTALTRACKS` and `TRACKTOTAL` into duplicate values.
- Track/disc tuples take the first number; exactly two slash-separated parts
  override separate totals (`gotaglib.go:157-167`). The previous shim's malformed
  tuple, first-value, unknown-format-overlay and empty-lyrics edge cases were
  corrected and have dedicated tests.
- Coded ID3 USLT/SYLT lyrics win over generic lyrics; generic lyrics become
  `lyrics:xxx` (`gotaglib.go:171-177,194-217`). Millisecond SYLT is rendered as LRC.
  Comments preserve line breaks; named ID3 comments remain `comment:<description>`.
  Native Vorbis `COMMENT` and `DESCRIPTION` remain separate keys: ffmpeg writes
  `DESCRIPTION` for its comment option. #16 later applies the alias list
  (`resources/mappings.yaml:117-118`).
- Normalized MP4 properties win over raw atoms, with the exact iTunes prefix
  stripped only at the raw overlay (`gotaglib.go:219-234`). Custom text atoms,
  TXXX, Vorbis/APE extension tags and multi-valued MusicBrainz role IDs survive.
  TIPL roles and TMCL musician credits are exposed in the Go vocabulary
  (`gotaglib.go:110-113,251-290`), not discarded by generic tag conversion.
- Embedded pictures include native APE cover-art items (which Lofty's generic
  `Tag::pictures` alone misses). The adapter returns presence, not image bytes
  (`gotaglib.go:115-116`).
- Cleaning/splitting/deduplication, tag vocabulary configuration, participant
  construction, dates, ReplayGain conversion and MediaFile mapping still belong
  to #16 (`model/metadata/metadata.go:205-237`, `map_mediafile.go:18-115`,
  `map_participants.go:21-76`). This crate must not pre-clean away raw values.

## Explicit gaps/deviations versus the Go path

These are **not** claims of complete TagLib equivalence:

1. **Format coverage:** ASF/WMA is rejected/skipped, so none of its tags,
   properties or pictures is available. Other TagLib formats outside the supported
   list likewise require a future backend. The old WMA golden is only a transform
   oracle, not evidence of WMA reading.
2. **AAC bit depth:** Lofty does not supply AAC-in-MP4 bit depth. It is 0 rather
   than Go's 16 for `tests/fixtures/test.m4a`. ALAC/FLAC bit depth is supplied when
   available; it is never inferred from a filename or from AAC decoding defaults.
3. **Bitrate estimator:** Lofty's audio bitrate is not always TagLib's rounded
   estimate. Existing FLAC fixture: 103 versus Go 104 kbps; Vorbis: nominal 16
   versus Go estimate 19 kbps. Vorbis without a positive nominal rate yields 0
   (the synthetic native-ffmpeg Vorbis fixture demonstrates this), not an invented
   container-overhead bitrate. Duration/channel/sample-rate parity is checked
   against all eight supported Go fixture types.
4. **Frame-timestamp SYLT:** MPEG-frame-based synchronized lyrics are skipped;
   only millisecond SYLT is converted to LRC. Lofty exposes frame counts but does
   not convert them to stream time. Other text/USLT lyrics remain available.
5. **Redundant MP4 raw binary aliases:** `covr`'s empty textual placeholder and
   `trkn:num`, `trkn:total`, `disk:num`, `disk:total` are not emitted. The actual
   information is present in `has_picture`, `tracknumber`/`tracktotal` and
   `discnumber`/`disctotal`. Arbitrary binary atoms are not serialized as text.
6. **Malformed empty-name freeform atoms:** the Go M4A fixture retains one value
   under the empty key, whereas Lofty preserves both (`["Test", "test"]`). This
   key is not a mapped metadata field. Unknown/conflicting exotic format tags
   beyond the oracle and regression cases are not guaranteed byte-identical.

Codec labels remain empty outside MPEG/MP4, matching the existing Go oracle
snapshots rather than guessing a codec from a container (notably WAV/AIFF).

## Tests and fixtures

`tests/fixtures/real/generate.sh` synthesizes 0.4s stereo sine-wave MP3, FLAC,
AAC/M4A and Vorbis/OGG with known Unicode/basic/tuple/comment/lyrics tags, plus a
2x2 PNG. Run it explicitly with bash and ffmpeg to regenerate; files are committed,
small, generated media with no external recording/license dependency. The native
Vorbis encoder is used because this machine's ffmpeg lacks libvorbis. Tests do
not require ffmpeg, never skip on its absence and never modify these fixtures.

`real_files.rs` reads these files directly through both public APIs.
`backend_semantics.rs` hand-encodes ID3 frames independently of Lofty, checks
ordered multi-values, comments/languages/SYLT, TIPL/TMCL, tuple/raw-atom precedence,
pictures in all four containers, malformed/missing files, no-sidecar fallback,
content sniffing, and filesystem-root confinement. Mutations use temporary copies.
`go_media.rs` reads eight original Go media fixtures and compares every expected
tag against the existing Go dump, allowing only the enumerated gaps above. It
also asserts all properties and image presence, with exact pinned differences for
AAC bit depth and FLAC/Vorbis bitrate.

The original seven Go-sidecar golden tests and five registry tests remain. Their
loader is explicitly an oracle utility; no runtime code invokes it. No fallback
can turn a missing/broken audio file into a valid track by supplying a sidecar.

To consume this in #16/#17, use the already-registered `taglib` constructor with
a filesystem root/base directory and relative media names; no sidecar producer
or global registration hook is needed. #16/#17 and application boot wiring remain
outside this change's ownership.
