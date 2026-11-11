//! HTTP Range + conditional-request planning — the Rust port of the header
//! half of Go's `http.ServeContent` (`net/http/fs.go`: `serveContent`
//! `:277-430`, `checkPreconditions` `:645-671`, `parseRange` `:1041-1121`,
//! `serveError` `:188-211`, `writeNotModified` `:627-643`). That is exactly the
//! contract Navidrome's seekable streaming path delegates to
//! (`core/stream/media_streamer.go:160-162`: `if s.Seekable() {
//! http.ServeContent(w, r, s.Name(), s.ModTime(), s) }`).
//!
//! The Rust port has no `net/http`, so this module is the standalone decision
//! table the HTTP layer (#27 `nd-http`/`nd-public`) calls before writing a
//! body. It is **pure**: metadata in, plan out; no filesystem, no socket, so
//! the whole matrix is testable without a server.
//!
//! ## Oracle
//!
//! Every asserted status/header below was produced by driving Go's real
//! `http.ServeContent` against `httptest.ResponseRecorder` on Go 1.27.3 and
//! recording the status and the surviving headers. The recorded table is the
//! authority for these rules; where it disagrees with the RFC text or with a
//! reviewer's recollection of `fs.go`, the oracle wins and the difference is
//! written down as a numbered deviation below rather than silently "fixed".
//!
//! ## Deliberate deviations from Go (each numbered, each RFC-legal)
//!
//! 1. **Multi-range ⇒ plain 200 with the whole content**, not
//!    `multipart/byteranges` (Go builds multipart at `fs.go:373-396`).
//!    RFC 9110 §14.2: a server MAY ignore a `Range` header, in which case it
//!    responds with a 200. Rationale: multipart framing per part is body
//!    construction we would have to synthesize and get byte-exact, and a
//!    client asking for N ranges gets N ranges of body back plus boundary
//!    overhead — amplification on a media endpoint where the same bytes are
//!    already reachable with one range per request. Clients that only
//!    understand 200 (players that probe with `bytes=0-1,-1000`) still play.
//!    **This is not Go parity**; it is carried on the #15 gap list, and the
//!    #27 fixtures must be authored against *this* behavior, not Go's.
//! 2. **Malformed `Range` ⇒ ignore ⇒ plain 200.** Go returns 416 with a
//!    `text/plain` body quoting the parse error
//!    (`parseRange` `:1063,:1078,:1089,:1093,:1098,:1107` → `serveError`
//!    `fs.go:203-206` → `http.Error` `server.go:2373-2392`). RFC 9110 §14.2
//!    instead says a recipient MUST ignore syntactically invalid range
//!    directives. The port follows the RFC: 416 is reserved for
//!    well-formed-but-unsatisfiable ranges, and no parser message reaches a
//!    client.
//! 3. **`bytes=-0` ⇒ 416.** Go computes `start = size`, `length = 0` and emits
//!    `206` with `Content-Range: bytes 10-9/10` — an end-before-start
//!    range-spec, which RFC 9110 §14.2 forbids. The port treats a zero-length
//!    suffix as unsatisfiable (416, `Content-Range: bytes */size`).
//! 4. **No ETag is ever synthesized.** Go reads whatever the caller set
//!    (`fs.go:506,:556,:595,:639` all consult `w.Header().Get("Etag")`) and
//!    never invents one; Navidrome's seekable path sets none
//!    (`media_streamer.go:161` passes only name + modTime). So
//!    [`FileMeta::etag`] is `Option<String>` and `None` means "no validator",
//!    which changes several outcomes (see [`preconditions`]).
//! 5. **Empty range items are skipped, then ignored if nothing remains.** Go
//!    skips empty items (`fs.go:1062-1064`) so `bytes=2-4,` still yields the
//!    valid 206 — the port matches that, and `bytes=`/`bytes=,` (nothing left)
//!    become plain 200s in both.
//! 6. **Content sniffing is not performed.** Go sniffs 512 bytes when the
//!    caller left `Content-Type` unset (`fs.go:287-299`) and rewinds. The port
//!    passes the caller's `content_type` through unchanged and documents that
//!    `None` means "the HTTP layer decides". Navidrome always knows the
//!    container (`media_streamer.go:97`/`:146`), so sniffing is dead weight
//!    here.
//!
//! Rules the oracle pins that are *easy to get wrong*, implemented exactly:
//!   - `size == 0` + a well-formed unsatisfiable Range ⇒ **200**, not 416
//!     (`fs.go:325-332`: the empty-file carve-out for clients that always send
//!     `Range`).
//!   - `sum(range lengths) > size` ⇒ ranges dropped ⇒ **200**
//!     (`fs.go:341-346`, Go's own anti-amplification guard). The port applies
//!     it **before** the multi-range branch so it covers the deviation-1 path
//!     too, i.e. a many-range request whose total exceeds the file is a 200 in
//!     both Go and this port.
//!   - more than 200 ranges (`defaultMaxContentRanges`, `fs.go:1024`) ⇒ header
//!     **ignored** ⇒ 200 (`fs.go:1055-1057`).
//!   - `If-Match` present but no server ETag ⇒ `etagStrongMatch` fails ⇒
//!     **412** (`fs.go:463-466` requires `a[0] == '"'` on both sides).
//!   - `If-None-Match` with no server ETag ⇒ weak match against `""` fails ⇒
//!     **200** (RFC 7232 §7.2: an unknown validator must not produce a 304).
//!   - `If-Unmodified-Since` is consulted **only** when `If-Match` is absent
//!     (`fs.go:651-655`).
//!   - `If-Modified-Since`/`If-Unmodified-Since` truncate mod-time to whole
//!     seconds and are GET/HEAD-only; zero mod-time or an unparseable date
//!     makes them inert (`condNone`, `fs.go:513-529`, `:563-582`).
//!   - `If-Range` date form requires **exact** `t.Unix() == modtime.Unix()`,
//!     not the truncated comparison IMS uses (`fs.go:585-614`).
//!   - `Accept-Ranges: bytes` is set on **both** 200 and 206
//!     (`fs.go:398`, before the status is chosen); error paths never set it.
//!   - 304 deletes Content-Type/Content-Length/Content-Encoding and deletes
//!     Last-Modified **when an ETag is set** (`fs.go:627-643`).
//!   - 412/416 clear Cache-Control/Content-Encoding/ETag/Last-Modified
//!     (`serveError`, `fs.go:196-206`).
//!   - `Content-Length` is set unconditionally on 200/206 (`fs.go:426-427`);
//!     Go skips it on a plain 200 only when the caller set `Content-Encoding`
//!     (`fs.go:426`), the gzip-middleware carve-out.

use std::fmt::Write as _;

/// `Accept-Ranges` for a seekable (direct-file) body — Go sets exactly this on
/// both 200 and 206 (`fs.go:398`).
pub const ACCEPT_RANGES_BYTES: &str = "bytes";

/// `Accept-Ranges` for a non-seekable transcoded stream
/// (`core/stream/media_streamer.go:165`).
pub const ACCEPT_RANGES_NONE: &str = "none";

/// Go's `defaultMaxContentRanges` (`fs.go:1024`): beyond this the header is
/// ignored outright (`fs.go:1055-1057`).
pub const MAX_CONTENT_RANGES: usize = 200;

/// A resolved byte range, Go's `httpRange` (`fs.go:1035-1038`): `start` plus
/// `length`, expressed here as inclusive `[start, end]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte (inclusive).
    pub start: u64,
    /// Last byte (inclusive). Always `>= start` in the port; Go can produce an
    /// empty range whose `end < start` for `bytes=-0` (deviation 3).
    pub end: u64,
}

impl ByteRange {
    /// Byte count, `end - start + 1` — Go's `httpRange.length`
    /// (`fs.go:1037`), set alongside `start` in `parseRange`.
    #[must_use]
    pub fn length(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// Why a `Range` header produced the outcome it did. Mirrors Go's three
/// `parseRange` results (`nil,nil` / ranges / `errNoOverlap`) plus the
/// `invalid range` error class the port turns into "ignored" (deviation 2).
/// Not `Copy`: [`RangeOutcome::Satisfied`] owns the resolved range list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeOutcome {
    /// No header at all — Go's `if s == "" { return nil, nil }`
    /// (`fs.go:1045-1047`).
    Absent,
    /// Header present and satisfied in full by the plain 200: either more than
    /// [`MAX_CONTENT_RANGES`] ranges (`fs.go:1055-1057`), the summed length
    /// exceeding the content (`fs.go:341-346`), or — deviation 2 — a
    /// syntactically invalid header that Go would have answered with 416.
    Ignored,
    /// Well-formed but no range overlaps the content: Go's `errNoOverlap`
    /// (`fs.go:1033,:1116-1119`), which `serveContent` turns into 416 unless
    /// the content is empty (`fs.go:325-332`).
    NoOverlap,
    /// At least one satisfiable range. `len == 1` is Go's single-range 206
    /// (`fs.go:355-368`); `len > 1` is Go's multipart path (`fs.go:373-396`),
    /// which this port serves as a plain 200 (deviation 1).
    Satisfied(Vec<ByteRange>),
}

/// `parseRange` (`fs.go:1041-1121`) plus `sumRangesSize` (`fs.go:1146-1151`),
/// resolved against a known `size`.
///
/// Accepted grammar is exactly Go's: the `bytes=` prefix (`fs.go:1048-1051`),
/// comma-separated byte-range-specs of the form `<first>-<last>`,
/// `<first>-`, or `-<suffix-length>` (`fs.go:1060-1113`). Empty items are
/// skipped, not errors (`fs.go:1062-1064`), so `bytes=,` and `bytes=` are
/// satisfied-by-200. Numbers must be plain non-negative decimal integers;
/// numbers must be plain non-negative decimal integers; signs, whitespace
/// inside a number, and any other unit are invalid and ignored here
/// (deviation 2). Whitespace around an item or around either bound is trimmed
/// exactly as Go trims it (`fs.go:1061,:1072`), so `bytes= 2-4` is valid.
#[must_use]
pub fn parse_range(header: Option<&str>, size: u64) -> RangeOutcome {
    let Some(raw) = header else {
        return RangeOutcome::Absent;
    };
    if raw.is_empty() {
        // Go: header not present (fs.go:1045-1047).
        return RangeOutcome::Absent;
    }

    // Go's `strings.HasPrefix(s, "bytes=")` (fs.go:1048-1051). A wrong unit is
    // an error in Go (→ 416); the port ignores it (deviation 2). Note Go does
    // not trim the header, so "  bytes=0-2" is invalid there too.
    let Some(spec) = raw.strip_prefix("bytes=") else {
        return RangeOutcome::Ignored;
    };

    // numRanges = strings.Count(spec, ",") + 1 (fs.go:1053), counted *before*
    // empty items are skipped — so "0-1,0-1," counts 3.
    let num_ranges = spec.matches(',').count() + 1;
    if num_ranges > MAX_CONTENT_RANGES {
        return RangeOutcome::Ignored;
    }

    let mut ranges: Vec<ByteRange> = Vec::new();
    let mut no_overlap = false;
    let mut invalid = false;

    for piece in spec.split(',') {
        let item = trim_http_ws(piece);
        if item.is_empty() {
            // Go: `continue` (fs.go:1062-1064).
            continue;
        }
        let Some((left, right)) = item.split_once('-') else {
            // Go: `strings.Cut` failed → "invalid range" (fs.go:1075-1078).
            invalid = true;
            break;
        };
        let left = trim_http_ws(left);
        let right = trim_http_ws(right);

        if left.is_empty() {
            // Suffix form `-<suffix-length>` (fs.go:1081-1096). Go rejects an
            // empty end or one that still starts with '-'.
            if right.is_empty() || right.starts_with('-') {
                invalid = true;
                break;
            }
            let Ok(n) = parse_u64(right) else {
                invalid = true;
                break;
            };
            if n == 0 || size == 0 {
                // Deviation 3. Go's oracle rows for these inputs:
                //   bytes=-0 on a 10-byte body → 206 Content-Range
                //     `bytes 10-9/10` (end before start, which RFC 9110 §14.2
                //     forbids)
                //   bytes=-3 on an empty body → 206 Content-Range
                //     `bytes 0--1/0`
                // Both are reported unsatisfiable here; the empty-content
                // carve-out (fs.go:325-332) then renders the empty case as a
                // plain 200 and the non-empty one as 416 + `bytes */size`.
                no_overlap = true;
                break;
            }
            // Go: `if i > size { i = size }`, `start = size - i`,
            // `length = size - start` (fs.go:1090-1095).
            let take = if n > size { size } else { n };
            ranges.push(ByteRange {
                start: size - take,
                end: size - 1,
            });
            continue;
        }

        let Ok(start) = parse_u64(left) else {
            invalid = true;
            break;
        };
        if start >= size {
            // Go: `noOverlap = true; continue` (fs.go:1100-1104). Note this is
            // only for the first-byte-pos form; the suffix form never sets it.
            no_overlap = true;
            continue;
        }
        if right.is_empty() {
            // Open-ended: `length = size - start` (fs.go:1106-1108).
            ranges.push(ByteRange {
                start,
                end: size - 1,
            });
            continue;
        }
        let Ok(last) = parse_u64(right) else {
            invalid = true;
            break;
        };
        if start > last {
            // Go: "invalid range" (fs.go:1110-1112). The port reports 200 via
            // deviation 2; Go reports 416.
            invalid = true;
            break;
        }
        // Go clamps `i >= size` to `size - 1` (fs.go:1113-1115).
        let end = if last >= size { size - 1 } else { last };
        ranges.push(ByteRange { start, end });
    }

    if invalid {
        return RangeOutcome::Ignored;
    }
    if ranges.is_empty() {
        // Go: `if noOverlap && len(ranges) == 0 { return errNoOverlap }`
        // (fs.go:1116-1119). With no items and no overlap flag (e.g.
        // `bytes=,`) Go returns an empty range list → plain 200.
        if no_overlap {
            return RangeOutcome::NoOverlap;
        }
        return RangeOutcome::Ignored;
    }

    // sumRangesSize > size → ranges = nil → plain 200 (fs.go:341-346). Applied
    // before the multi-range decision so deviation 1 cannot be used to pull
    // more bytes out of the file than the file contains.
    let mut sum: u64 = 0;
    for r in &ranges {
        sum = sum.saturating_add(r.length());
    }
    if sum > size {
        return RangeOutcome::Ignored;
    }

    RangeOutcome::Satisfied(ranges)
}

/// Strict unsigned decimal parse, standing in for Go's
/// `strconv.ParseInt(s, 10, 64)` (`fs.go:1088,:1099,:1110`): plain ASCII
/// digits only, no sign, no whitespace, no overflow.
fn parse_u64(text: &str) -> Result<u64, ()> {
    if text.is_empty() {
        return Err(());
    }
    let mut acc: u64 = 0;
    for ch in text.chars() {
        let d = match ch {
            '0'..='9' => u64::from(ch as u8 - b'0'),
            _ => return Err(()),
        };
        acc = acc
            .checked_mul(10)
            .and_then(|v| v.checked_add(d))
            .ok_or(())?;
    }
    Ok(acc)
}

/// Go's `textproto.TrimString` (`net/http/textproto/strings.go:14-17`): trim
/// ASCII space and tab only — not `\n`, `\r`, or non-ASCII whitespace.
///
/// Oracle note: `serveContent` trims the whole range item (`fs.go:1061`) and
/// then each half of the cut (`fs.go:1072`), so `bytes= 2-4`, `bytes=2 -4` and
/// `bytes=\t2-4` are all **valid** 206s in Go. Whitespace only fails when it
/// sits *inside* a number (`bytes=1 2-3`), where Go's `strconv.ParseInt`
/// errors.
#[must_use]
pub fn trim_http_ws(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut end = bytes.len();
    while start < end && (bytes[start] == b' ' || bytes[start] == b'\t') {
        start += 1;
    }
    while end > start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t') {
        end -= 1;
    }
    &text[start..end]
}

/// The metadata a body is served from — Go's `sizeFunc()` result plus the
/// `modTime` argument of `http.ServeContent`
/// (`core/stream/media_streamer.go:161`), plus the ETag the caller chose to
/// set on the writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    /// Total content length in bytes (`fs.go:309-315`). For a direct file this
    /// is the real size; for a transcoded pipe there is no size at all — see
    /// [`plan_streaming`].
    pub size: u64,
    /// Last-modified as **epoch seconds** (crate-wide "Go `time.Time` → epoch
    /// seconds" divergence, `lib.rs`). `None` = Go's zero time: no
    /// `Last-Modified` header and the time-based conditionals are inert
    /// (`isZeroTime`, `fs.go:616-621`, applied at `:523,:566,:599`).
    pub last_modified_secs: Option<i64>,
    /// The caller's `ETag` **in wire form**, quotes included: `"abc"` (strong)
    /// or `W/"abc"` (weak). `None` = the caller set no validator, which is
    /// Navidrome's seekable path (`media_streamer.go:161` sets none). The port
    /// never synthesizes one — deviation 4.
    pub etag: Option<String>,
    /// The caller's `Content-Type`, passed through unchanged. `None` means the
    /// HTTP layer chooses it; Go would sniff 512 bytes here (`fs.go:287-299`),
    /// which this port does not (deviation 6).
    pub content_type: Option<String>,
    /// Go's `Godebug`-free equivalent of the caller having set
    /// `Content-Encoding`: on a plain 200 Go then omits `Content-Length`
    /// (`fs.go:426`) so a gzip wrapper keeps working. A 206 always sets it.
    pub content_encoding: Option<String>,
}

impl FileMeta {
    /// `isZeroTime` (`fs.go:616-621`): zero or epoch-0 means "unknown", so the
    /// conditional timestamps are inert and `Last-Modified` is not written
    /// (`setLastModified`, `fs.go:623-627`).
    #[must_use]
    pub fn has_known_mtime(&self) -> bool {
        match self.last_modified_secs {
            Some(secs) => secs != 0,
            None => false,
        }
    }

    /// `w.Header().Get("Etag")` — the empty string when unset, because Go's
    /// `etagStrongMatch`/`etagWeakMatch` compare against exactly that
    /// (`fs.go:506,:556,:595`).
    #[must_use]
    pub fn etag_or_empty(&self) -> String {
        self.etag.clone().unwrap_or_default()
    }
}

/// A conditional/Range request, flattened from headers by the caller.
/// `None` = header absent/empty (Go treats an empty header value as absent in
/// every `check*` function).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CondRequest {
    /// HTTP method. Go compares exactly against `"GET"`/`"HEAD"`
    /// (`fs.go:564,:586,:661,:429`); the port matches case-insensitively since
    /// HTTP methods are case-sensitive but proxies are not always faithful.
    pub method: String,
    /// Raw `Range` value.
    pub range: Option<String>,
    /// Raw `If-Match` value (`*` or a comma-separated tag list).
    pub if_match: Option<String>,
    /// Raw `If-None-Match` value (`*` or a comma-separated tag list).
    pub if_none_match: Option<String>,
    /// Raw `If-Modified-Since` value (an HTTP date string).
    pub if_modified_since: Option<String>,
    /// Raw `If-Unmodified-Since` value (an HTTP date string).
    pub if_unmodified_since: Option<String>,
    /// Raw `If-Range` value (an entity tag or an HTTP date).
    pub if_range: Option<String>,
}

impl CondRequest {
    /// `r.Method == "GET" || r.Method == "HEAD"`, the guard on
    /// `checkIfModifiedSince` (`fs.go:564-566`), `checkIfRange`
    /// (`fs.go:586-588`), and the 304-vs-412 choice in `checkPreconditions`
    /// (`fs.go:657-665`).
    #[must_use]
    pub fn is_get_or_head(&self) -> bool {
        self.method.eq_ignore_ascii_case("GET") || self.is_head()
    }

    /// `r.Method == http.MethodHead` (`fs.go:429`: Go writes the headers but
    /// skips the body copy).
    #[must_use]
    pub fn is_head(&self) -> bool {
        self.method.eq_ignore_ascii_case("HEAD")
    }
}

/// Go's `condResult` (`fs.go:475-481`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondResult {
    /// `condNone`: the validator was absent, inert, or unparseable.
    None,
    /// `condTrue`: the precondition holds, keep going.
    True,
    /// `condFalse`: the precondition failed.
    False,
}

/// `scanETag` (`fs.go:437-460`): pull a syntactically valid entity tag off the
/// front of `s`, returning the tag and the remainder. Returns `None` when no
/// valid tag is present, exactly as Go returns `("", "")`.
///
/// Allowed tag characters are Go's set (`fs.go:450`): `0x21`, `0x23-0x7E`, and
/// `>= 0x80` — i.e. printable ASCII **except** `"` itself and `"`-delimited
/// content may not contain a raw quote.
#[must_use]
pub fn scan_etag(s: &str) -> Option<(&str, &str)> {
    let s = trim_http_ws(s);
    let bytes = s.as_bytes();
    let mut start = 0usize;
    if s.starts_with("W/") {
        start = 2;
    }
    let rest = &bytes[start..];
    if rest.len() < 2 || rest[0] != b'"' {
        return None;
    }
    let mut i = start + 1;
    while i < bytes.len() {
        let c = bytes[i];
        let allowed = c == 0x21 || (0x23..=0x7E).contains(&c) || c >= 0x80;
        if c == b'"' {
            return Some((&s[..=i], &s[i + 1..]));
        }
        if !allowed {
            return None;
        }
        i += 1;
    }
    None
}

/// `etagStrongMatch` (`fs.go:463-466`), verbatim: `a == b && a != "" && a[0] == '"'`.
/// Both arguments are wire-form tags. A weak server tag therefore never
/// matches a strong request tag, and an absent server tag (`""`) never matches
/// anything.
#[must_use]
pub fn etag_strong_match(a: &str, b: &str) -> bool {
    a == b && !a.is_empty() && a.starts_with('"')
}

/// `etagWeakMatch` (`fs.go:469-472`): compare after stripping the `W/` prefix
/// from both sides. Weakness is ignored, so `W/"x"` matches `"x"`.
#[must_use]
pub fn etag_weak_match(a: &str, b: &str) -> bool {
    strip_weak_prefix(a) == strip_weak_prefix(b)
}

/// Go's `strings.TrimPrefix(s, "W/")`. Go is case-sensitive here, so `w/"x"`
/// is not stripped; the port keeps that exactly.
fn strip_weak_prefix(s: &str) -> String {
    match s.strip_prefix("W/") {
        Some(rest) => rest.to_string(),
        None => s.to_string(),
    }
}

/// `checkIfMatch` (`fs.go:483-511`): `*` matches anything; otherwise walk the
/// comma-separated tag list and require a **strong** match against the server
/// tag. An unparseable token stops the scan (Go `break`) rather than erroring,
/// so `If-Match: abc` never matches. With no server tag every comparison
/// fails ⇒ [`CondResult::False`] ⇒ 412.
#[must_use]
pub fn check_if_match(server_etag: &str, header: Option<&str>) -> CondResult {
    let Some(mut im) = header else {
        return CondResult::None;
    };
    if im.is_empty() {
        return CondResult::None;
    }
    loop {
        im = trim_http_ws(im);
        if im.is_empty() {
            break;
        }
        if im.starts_with(',') {
            im = &im[1..];
            continue;
        }
        if im.starts_with('*') {
            return CondResult::True;
        }
        let Some((etag, remain)) = scan_etag(im) else {
            break;
        };
        if etag_strong_match(etag, server_etag) {
            return CondResult::True;
        }
        im = remain;
    }
    CondResult::False
}

/// `checkIfUnmodifiedSince` (`fs.go:513-530`): inert when the header is empty,
/// the mod-time is zero, or the date is unparseable. Otherwise the truncated
/// mod-time must be `<= t`. Not GET/HEAD-guarded in Go — this port matches
/// that (only IMS and If-Range carry the method guard).
#[must_use]
pub fn check_if_unmodified_since(mtime_secs: Option<i64>, header: Option<&str>) -> CondResult {
    let Some(ius) = header else {
        return CondResult::None;
    };
    if ius.is_empty() || !has_mtime(mtime_secs) {
        return CondResult::None;
    }
    let Some(t) = parse_http_time(ius) else {
        return CondResult::None;
    };
    // modtime.Truncate(time.Second) — for epoch seconds this is the identity,
    // documented rather than silently omitted.
    let truncated = mtime_secs.unwrap_or(0);
    if truncated <= t {
        CondResult::True
    } else {
        CondResult::False
    }
}

/// `checkIfNoneMatch` (`fs.go:532-561`): `*` fails unconditionally (Go returns
/// `condFalse` for `*` here, unlike If-Match — RFC 7232 §7.2: `*` matches any
/// *current* representation, so for a conditional GET it means "not
/// modified"). Otherwise a **weak** match against the server tag fails the
/// precondition. With no server tag, weak-matching against `""` never
/// succeeds ⇒ [`CondResult::None`]… precisely: it falls through the loop and
/// returns `condTrue`, which is why Go answers 200, not 304.
#[must_use]
pub fn check_if_none_match(server_etag: &str, header: Option<&str>) -> CondResult {
    let Some(inm) = header else {
        return CondResult::None;
    };
    if inm.is_empty() {
        return CondResult::None;
    }
    let mut buf = inm;
    loop {
        buf = trim_http_ws(buf);
        if buf.is_empty() {
            break;
        }
        if buf.starts_with(',') {
            buf = &buf[1..];
            continue;
        }
        if buf.starts_with('*') {
            return CondResult::False;
        }
        let Some((etag, remain)) = scan_etag(buf) else {
            break;
        };
        if etag_weak_match(etag, server_etag) {
            return CondResult::False;
        }
        // Go advances past the consumed tag (`buf = remain`, fs.go:556-559).
        buf = remain;
    }
    CondResult::True
}

/// `checkIfModifiedSince` (`fs.go:563-582`): GET/HEAD only, inert on empty
/// header or zero mod-time or unparseable date, and 304-worthy when the
/// truncated mod-time is `<= t`.
#[must_use]
pub fn check_if_modified_since(
    method_get_or_head: bool,
    mtime_secs: Option<i64>,
    header: Option<&str>,
) -> CondResult {
    if !method_get_or_head {
        return CondResult::None;
    }
    let Some(ims) = header else {
        return CondResult::None;
    };
    if ims.is_empty() || !has_mtime(mtime_secs) {
        return CondResult::None;
    }
    let Some(t) = parse_http_time(ims) else {
        return CondResult::None;
    };
    let truncated = mtime_secs.unwrap_or(0);
    if truncated <= t {
        CondResult::False
    } else {
        CondResult::True
    }
}

/// `checkIfRange` (`fs.go:585-614`): GET/HEAD only. The tag form requires a
/// **strong** match against the server tag; failing it is `condFalse`, which
/// `checkPreconditions` turns into "drop the Range header" ⇒ plain 200. The
/// date form requires **exact** second equality with the mod-time (`t.Unix()
/// == modtime.Unix()`), no truncation, and is `condFalse` when the mod-time is
/// zero or the value is unparseable.
#[must_use]
pub fn check_if_range(
    method_get_or_head: bool,
    server_etag: &str,
    mtime_secs: Option<i64>,
    header: Option<&str>,
) -> CondResult {
    if !method_get_or_head {
        return CondResult::None;
    }
    let Some(ir) = header else {
        return CondResult::None;
    };
    if ir.is_empty() {
        return CondResult::None;
    }
    if let Some((etag, _)) = scan_etag(ir) {
        if etag_strong_match(etag, server_etag) {
            return CondResult::True;
        }
        return CondResult::False;
    }
    // Date form (Go comment at fs.go:604: "may also be the modtime date").
    if !has_mtime(mtime_secs) {
        return CondResult::False;
    }
    let Some(t) = parse_http_time(ir) else {
        return CondResult::False;
    };
    if t == mtime_secs.unwrap_or(0) {
        CondResult::True
    } else {
        CondResult::False
    }
}

/// `isZeroTime` (`fs.go:616-621`): zero or Unix-epoch-0 both mean unspecified.
fn has_mtime(mtime_secs: Option<i64>) -> bool {
    match mtime_secs {
        Some(secs) => secs != 0,
        None => false,
    }
}

/// `checkPreconditions` (`fs.go:645-671`): the RFC 7232 §6 precondition order,
/// ported as an outcome instead of Go's `(done bool, rangeHeader string)`
/// return. [`PrecondOutcome::NotModified`] and
/// [`PrecondOutcome::PreconditionFailed`] mean the response is already
/// answered, so no body and no Range handling follows;
/// [`PrecondOutcome::Pass`] hands back the Range header that `serveContent`
/// should feed to [`parse_range`] (Go's second return value, `fs.go:669-670`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrecondOutcome {
    /// `w.WriteHeader(StatusPreconditionFailed)` (`fs.go:653`): If-Match or
    /// If-Unmodified-Since failed, or If-None-Match failed on a method that is
    /// not GET/HEAD (`fs.go:663-665`).
    PreconditionFailed,
    /// `writeNotModified` (`fs.go:657-662,:666-668`): If-None-Match or
    /// If-Modified-Since hit on GET/HEAD.
    NotModified,
    /// Preconditions passed; `serveContent` goes on to handle the Range.
    Pass {
        /// The Range header to honor: `None` when no Range was sent, or when
        /// `If-Range` failed and Go drops it (`fs.go:669-670`).
        range: Option<String>,
    },
}

/// Evaluate If-Match → If-Unmodified-Since → If-None-Match →
/// If-Modified-Since → If-Range in Go's order and report the outcome.
#[must_use]
pub fn preconditions(meta: &FileMeta, req: &CondRequest) -> PrecondOutcome {
    let etag = meta.etag_or_empty();

    // If-Match first; If-Unmodified-Since only when If-Match is absent
    // (fs.go:649-652).
    let mut ch = check_if_match(etag.as_str(), req.if_match.as_deref());
    if ch == CondResult::None {
        ch = check_if_unmodified_since(meta.last_modified_secs, req.if_unmodified_since.as_deref());
    }
    if ch == CondResult::False {
        // Go: `w.WriteHeader(StatusPreconditionFailed)` — no body, and
        // serveError's header clearing does NOT run on this path (fs.go:652-655).
        return PrecondOutcome::PreconditionFailed;
    }

    match check_if_none_match(etag.as_str(), req.if_none_match.as_deref()) {
        CondResult::False => {
            if req.is_get_or_head() {
                return PrecondOutcome::NotModified;
            }
            return PrecondOutcome::PreconditionFailed;
        }
        CondResult::None => {
            if check_if_modified_since(
                req.is_get_or_head(),
                meta.last_modified_secs,
                req.if_modified_since.as_deref(),
            ) == CondResult::False
            {
                return PrecondOutcome::NotModified;
            }
        }
        CondResult::True => {}
    }

    // If-Range only consulted when a Range header is present (fs.go:669-670).
    let mut range = req.range.clone();
    if range.is_some()
        && check_if_range(
            req.is_get_or_head(),
            etag.as_str(),
            meta.last_modified_secs,
            req.if_range.as_deref(),
        ) == CondResult::False
    {
        range = None;
    }
    PrecondOutcome::Pass { range }
}

/// The headers and byte window the HTTP layer must emit. Every `Option` maps
/// to "set this header" / `None` = "do not send it", so the pruning rules from
/// `writeNotModified` (`fs.go:627-643`) and `serveError` (`fs.go:196-206`) are
/// expressible without the caller guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServePlan {
    /// 200, 206, 304, 412 or 416.
    pub status: u16,
    /// First byte to send (inclusive). Zero when no body is sent.
    pub start: u64,
    /// Last byte to send (inclusive).
    pub end: u64,
    /// `Content-Length`, `None` = header must not be sent (304, 412, 416, and
    /// the `Content-Encoding` carve-out on a plain 200 — `fs.go:426`).
    pub content_length: Option<u64>,
    /// `Content-Range`: set for 206 and 416, cleared everywhere else.
    pub content_range: Option<String>,
    /// `Accept-Ranges`: `bytes` on 200/206 (`fs.go:398`), absent on error
    /// statuses.
    pub accept_ranges: Option<String>,
    /// `Last-Modified`: set on 200/206 when the mod-time is known
    /// (`setLastModified`, `fs.go:623-627`); deleted on 304 when an ETag is
    /// set (`fs.go:639-641`); cleared on 412/416 (`fs.go:199,:201`).
    pub last_modified: Option<String>,
    /// `ETag` in wire form, carried through on 200/206/304 and cleared on the
    /// error paths (`fs.go:200`).
    pub etag: Option<String>,
    /// `Content-Type`. On 304 it is deleted (`fs.go:634`); on an error status
    /// Go replaces it with `text/plain; charset=utf-8` for the message body
    /// (`http.Error`, `server.go:2387`), which this field reports as `None`
    /// because the port sends no error body (see [`error_message`]).
    pub content_type: Option<String>,
    /// True when no body may be written (304, 412, 416, or HEAD — Go skips the
    /// copy for HEAD at `fs.go:429-431`).
    pub no_body: bool,
    /// Go's `http.Error` message text on the 416 path
    /// (`serveError(w, err.Error(), StatusRequestedRangeNotSatisfiable)`,
    /// `fs.go:203-206`). The port exposes the text so the HTTP layer can
    /// reproduce Go's body if it wants one; it is `None` for every other
    /// status. 412 has no message in Go (`WriteHeader` only, `fs.go:653`).
    pub error_message: Option<String>,
}

impl ServePlan {
    /// Bytes to write: 0 whenever [`ServePlan::no_body`] is set, else the
    /// declared `Content-Length`. A body reader must use this, not `end`, to
    /// decide whether to read: an empty file plans `start = 0, end = 0` with
    /// `Content-Length: 0`, so `end` alone would describe a one-byte window.
    #[must_use]
    pub fn bytes_to_write(&self) -> u64 {
        if self.no_body {
            0
        } else {
            self.content_length.unwrap_or(0)
        }
    }
}

/// Evaluate preconditions + Range for a **seekable** body of known size — the
/// direct-file path (`media_streamer.go:160-162`).
///
/// This is `serveContent` (`fs.go:277-430`) minus content sniffing
/// (deviation 6): preconditions first (`:282`), then `parseRange` (`:322`),
/// the empty-file carve-out (`:325-332`), the summed-size guard (`:341-346`),
/// the single/multi decision (`:349-396`), and finally `Accept-Ranges`,
/// `Content-Length`, and the status write (`:398-431`).
#[must_use]
pub fn plan(meta: &FileMeta, req: &CondRequest) -> ServePlan {
    let last_modified = if meta.has_known_mtime() {
        Some(http_date(meta.last_modified_secs.unwrap_or(0)))
    } else {
        None
    };

    match preconditions(meta, req) {
        PrecondOutcome::PreconditionFailed => {
            // Go: `w.WriteHeader(412)` with whatever headers the caller had
            // already set. serveError is NOT called, so Etag/Last-Modified
            // survive (oracle: `if-match-miss … Last-Modified=… Etag="abc"`).
            // No body, no Content-Length, no Accept-Ranges, no Content-Range.
            ServePlan {
                status: 412,
                start: 0,
                end: 0,
                content_length: None,
                content_range: None,
                accept_ranges: None,
                last_modified: last_modified.clone(),
                etag: meta.etag.clone(),
                content_type: None,
                no_body: true,
                error_message: None,
            }
        }
        PrecondOutcome::NotModified => {
            // writeNotModified (fs.go:627-643): delete Content-Type,
            // Content-Length, Content-Encoding; delete Last-Modified when an
            // ETag is set. Everything the caller set that is not listed
            // survives, which is why Etag is carried through.
            ServePlan {
                status: 304,
                start: 0,
                end: 0,
                content_length: None,
                content_range: None,
                accept_ranges: None,
                last_modified: if meta.etag.is_some() {
                    None
                } else {
                    last_modified
                },
                etag: meta.etag.clone(),
                content_type: None,
                no_body: true,
                error_message: None,
            }
        }
        PrecondOutcome::Pass { range } => {
            let mut plan = ServePlan {
                status: 200,
                start: 0,
                end: meta.size.saturating_sub(1),
                content_length: Some(meta.size),
                content_range: None,
                accept_ranges: Some(ACCEPT_RANGES_BYTES.to_string()),
                last_modified,
                etag: meta.etag.clone(),
                content_type: meta.content_type.clone(),
                no_body: false,
                error_message: None,
            };

            let outcome = parse_range(range.as_deref(), meta.size);
            match outcome {
                RangeOutcome::NoOverlap => {
                    if meta.size == 0 {
                        // fs.go:325-332: empty content + Range → ignore → 200.
                        // The 200 plan above is already correct.
                        finalize(&mut plan, req.is_head(), meta);
                        return plan;
                    }
                    // fs.go:333-338: Content-Range is set, then serveError runs
                    // (clearing Cache-Control/Content-Encoding/Etag/
                    // Last-Modified) and http.Error writes the message.
                    plan.status = 416;
                    plan.start = 0;
                    plan.end = 0;
                    plan.content_length = None;
                    plan.content_range = Some(format!("bytes */{}", meta.size));
                    plan.accept_ranges = None;
                    plan.last_modified = None;
                    plan.etag = None;
                    plan.content_type = None;
                    plan.no_body = true;
                    plan.error_message =
                        Some(error_message("invalid range: failed to overlap").to_string());
                    return plan;
                }
                RangeOutcome::Satisfied(ranges) => {
                    if ranges.len() == 1 {
                        let ra = ranges[0];
                        plan.status = 206;
                        plan.start = ra.start;
                        plan.end = ra.end;
                        plan.content_length = Some(ra.length());
                        plan.content_range = Some(content_range(&ra, meta.size));
                    }
                    // len > 1: deviation 1 → plain 200 full content (the plan
                    // above already describes it).
                }
                RangeOutcome::Ignored | RangeOutcome::Absent => {}
            }
            finalize(&mut plan, req.is_head(), meta);
            plan
        }
    }
}

/// Plan for a **non-seekable** transcoded body: no size, no ranges. Mirrors
/// Go's non-seekable branch (`core/stream/media_streamer.go:165-197`), which
/// sets `Accept-Ranges: none`, writes `Content-Type`, optionally writes an
/// estimated `Content-Length` from the `estimateContentLength` request
/// parameter (`:168-172`), and for HEAD drains the stream without writing
/// bytes (`:174-177`).
///
/// Preconditions are evaluated against the media file's own `UpdatedAt`/etag so
/// a revalidating share link stays cheap; a `Range` on a pipe cannot be
/// honored, so it is ignored ⇒ 200 (`Accept-Ranges: none` tells the client
/// ranges are not supported).
#[must_use]
pub fn plan_streaming(
    meta: &FileMeta,
    req: &CondRequest,
    estimated_content_length: Option<u64>,
) -> ServePlan {
    let last_modified = if meta.has_known_mtime() {
        Some(http_date(meta.last_modified_secs.unwrap_or(0)))
    } else {
        None
    };

    match preconditions(meta, req) {
        PrecondOutcome::PreconditionFailed => ServePlan {
            status: 412,
            start: 0,
            end: 0,
            content_length: None,
            content_range: None,
            accept_ranges: None,
            last_modified,
            etag: meta.etag.clone(),
            content_type: None,
            no_body: true,
            error_message: None,
        },
        PrecondOutcome::NotModified => ServePlan {
            status: 304,
            start: 0,
            end: 0,
            content_length: None,
            content_range: None,
            accept_ranges: None,
            last_modified: if meta.etag.is_some() {
                None
            } else {
                last_modified
            },
            etag: meta.etag.clone(),
            content_type: None,
            no_body: true,
            error_message: None,
        },
        PrecondOutcome::Pass { .. } => {
            // No size on a pipe: Content-Length only when the caller asked for
            // the estimate (media_streamer.go:168-172).
            let head = req.is_head();
            let mut plan = ServePlan {
                status: 200,
                start: 0,
                end: 0,
                content_length: if head { None } else { estimated_content_length },
                content_range: None,
                accept_ranges: Some(ACCEPT_RANGES_NONE.to_string()),
                last_modified,
                etag: meta.etag.clone(),
                content_type: meta.content_type.clone(),
                no_body: head,
                error_message: None,
            };
            // Go's non-seekable arm writes its own headers and then calls
            // `w.ServeContent` (`serve.go:489-492`), so the `fs.go:425-427`
            // Content-Encoding carve-out applies to the estimate as well.
            finalize(&mut plan, req.is_head(), meta);
            plan
        }
    }
}

/// Apply the two tail rules of `serveContent`: HEAD writes headers but no body
/// (`fs.go:429-431`), and a plain 200 omits `Content-Length` when the caller
/// set `Content-Encoding` (`fs.go:426`). A 206 always sets it.
fn finalize(plan: &mut ServePlan, is_head: bool, meta: &FileMeta) {
    if plan.status == 200 && meta.content_encoding.is_some() {
        plan.content_length = None;
    }
    if is_head {
        plan.no_body = true;
    }
}

/// `httpRange.contentRange` (`fs.go:1013-1016` region: `contentRange(size)`
/// builds `"bytes start-end/size"`).
fn content_range(range: &ByteRange, size: u64) -> String {
    format!("bytes {}-{}/{}", range.start, range.end, size)
}

/// Go's `http.Error` body prefix (`server.go:2389-2391` writes
/// `<code> message: <text>\n`). Reported without the wrapper so the HTTP layer
/// can format it however it serves errors.
#[must_use]
pub fn error_message(text: &str) -> String {
    text.to_string()
}

/// Render epoch seconds as Go's `TimeFormat` (`net/http/server.go:1027`:
/// `"Mon, 02 Jan 2006 15:04:05" GMT`), the exact value `setLastModified`
/// writes (`fs.go:623-627`, `modtime.UTC().Format(TimeFormat)`).
#[must_use]
pub fn http_date(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    // WEEKDAYS is indexed from Monday, and 1970-01-01 (day 0) was a
    // Thursday, so the epoch needs a +3 offset. Rust `%` is remainder
    // (negative for negative operands), hence the +7 before the second %.
    let weekday = ((days % 7) + 7 + 3) % 7;
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    let mut out = String::with_capacity(29);
    let _ = write!(
        out,
        "{wd}, {day:02} {mon} {year:04} {hour:02}:{minute:02}:{second:02} GMT",
        wd = WEEKDAYS[weekday as usize],
        mon = MONTHS[month as usize - 1]
    );
    out
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Month abbreviation → 1-based month number.
fn month_num(name: &str) -> Option<u32> {
    MONTHS.iter().position(|m| *m == name).map(|i| i as u32 + 1)
}

fn weekday_index(name: &str) -> Option<usize> {
    WEEKDAYS.iter().position(|w| *w == name)
}

/// `http.ParseTime` (`net/http/header.go:126-137`) over Go's `timeFormats`
/// (`header.go:120-124`).
///
/// The oracle corrects a common recollection here: Go's `timeFormats` is
/// `[TimeFormat, time.RFC850, time.ANSIC]` — **three** layouts, not four.
/// `http.ParseTime` is `time.Parse` over those three; there is no "RFC1123 with
/// dashes" layout, and `Sat, 06-Nov-2021 19:10:15 GMT` **fails** to parse
/// (verified: it falls through to ANSIC and errors on the comma). Accepted
/// inputs, all pinned by tests against the Go oracle:
///   1. `Mon, 02 Jan 2006 15:04:05 GMT` — and Go ignores a wrong weekday, so
///      `Tue, 06 Nov 2021 19:10:15 GMT` parses too. `UTC` is Go's name for GMT,
///      so it parses as well.
///   2. `Sunday, 06-Nov-21 19:10:15 GMT` (RFC 850) — two-digit year, with Go's
///      century rule from `time/format.go:1100-1105`: `year += 1900` if
///      `year >= 69`, else `+= 2000`.
///   3. `Mon Jan _2 15:04:05 2006` (ANSI C `asctime`) — day may be
///      space-padded or zero-padded; year is last.
///
/// Anything else returns `None`, which every caller treats as
/// [`CondResult::None`] / "ignore the header" — never as an error. Zones are
/// resolved as UTC: Go's `time.Parse` maps `UTC`→`GMT`, and a bare zone name
/// (`MST` in layout-parsed input) is an unspecified zone whose `Unix()` equals
/// the UTC reading, which is what the oracle shows
/// (`Saturday, 06-Nov-21 19:10:15 MST` → `unix=1636225815`, the same as GMT).
#[must_use]
pub fn parse_http_time(text: &str) -> Option<i64> {
    if let Some(secs) = parse_rfc1123_gmt(text) {
        return Some(secs);
    }
    if let Some(secs) = parse_rfc850(text) {
        return Some(secs);
    }
    parse_ansic(text)
}

fn to_epoch(days: i64, hour: i64, minute: i64, second: i64) -> Option<i64> {
    days.checked_mul(86_400)?.checked_add(
        hour.checked_mul(3600)?
            .checked_add(minute.checked_mul(60)?.checked_add(second)?)?,
    )
}

/// Layout 1: `Mon, 02 Jan 2006 15:04:05 GMT`.
fn parse_rfc1123_gmt(text: &str) -> Option<i64> {
    let (head, tail) = text.split_once(',')?;
    weekday_index(trim_http_ws(head))?;
    let parts: Vec<&str> = tail.split_whitespace().collect();
    // Expected: day, month, year, time, zone.
    if parts.len() != 5 {
        return None;
    }
    let day: i64 = parse_u64(parts[0]).ok()? as i64;
    let month = month_num(parts[1])?;
    let year: i64 = parse_i64(parts[2])?;
    let (hour, minute, second) = parse_hms(parts[3])?;
    // Go's TimeFormat literal ends with exactly "GMT" (server.go:1027).
    // Oracle: http.ParseTime("Sun, 06 Nov 2021 19:10:15 UTC") **errors**, and
    // the matching If-Modified-Since request answers 200 (condNone). UTC is
    // therefore not accepted in this layout; ANSIC has no zone field at all,
    // which is why that layout reads as UTC.
    if parts[4] != "GMT" {
        return None;
    }
    if !(1..=31).contains(&day) {
        return None;
    }
    to_epoch(
        days_from_civil(year, month, day as u32),
        hour,
        minute,
        second,
    )
}

/// Layout 2 (RFC 850): `Sunday, 06-Nov-21 19:10:15 GMT`.
fn parse_rfc850(text: &str) -> Option<i64> {
    let (head, tail) = text.split_once(',')?;
    let long_weekday = trim_http_ws(head);
    const LONG_WEEKDAYS: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    if !LONG_WEEKDAYS.contains(&long_weekday) {
        return None;
    }
    let parts: Vec<&str> = tail.split_whitespace().collect();
    if parts.len() != 3 {
        return None;
    }
    // date token: dd-Mmm-yy
    let mut it = parts[0].split('-');
    let day_s = it.next()?;
    let mon_s = it.next()?;
    let year_s = it.next()?;
    if it.next().is_some() {
        return None;
    }
    let day: i64 = parse_u64(day_s).ok()? as i64;
    let month = month_num(mon_s)?;
    let year_short: i64 = parse_u64(year_s).ok()? as i64;
    // time/format.go:1100-1105: two-digit years >= 69 are 19xx, else 20xx.
    let year = if year_short >= 69 {
        1900 + year_short
    } else {
        2000 + year_short
    };
    let (hour, minute, second) = parse_hms(parts[1])?;
    // Go's RFC850 layout ends with the literal "MST", i.e. an unspecified
    // zone that reads as UTC. Any zone token is accepted, matching Go's
    // behavior for that layout.
    if !(1..=31).contains(&day) {
        return None;
    }
    to_epoch(
        days_from_civil(year, month, day as u32),
        hour,
        minute,
        second,
    )
}

/// Layout 3 (ANSI C asctime): `Mon Jan _2 15:04:05 2006`.
fn parse_ansic(text: &str) -> Option<i64> {
    let parts: Vec<&str> = split_ansic(text);
    if parts.len() != 5 {
        return None;
    }
    weekday_index(parts[0])?;
    let month = month_num(parts[1])?;
    let day: i64 = parse_u64(parts[2]).ok()? as i64;
    let (hour, minute, second) = parse_hms(parts[3])?;
    let year: i64 = parse_i64(parts[4])?;
    if !(1..=31).contains(&day) {
        return None;
    }
    to_epoch(
        days_from_civil(year, month, day as u32),
        hour,
        minute,
        second,
    )
}

/// Go's `asctime` layout is `Mon Jan _2 15:04:05 2006`, where `_2` is a
/// space-padded day. Split on single spaces — so `Jan  6` yields an empty
/// token at the day position — and collapse exactly that one empty token,
/// which keeps `Jan 6` and `Jan  6` both parseable while a stray double space
/// anywhere else still fails to yield the five expected fields.
fn split_ansic(text: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = text.split(' ').collect();
    if parts.len() == 6 && parts[2].is_empty() {
        parts.remove(2);
    }
    parts
}

fn parse_hms(text: &str) -> Option<(i64, i64, i64)> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hour = parse_u64(parts[0]).ok()? as i64;
    let minute = parse_u64(parts[1]).ok()? as i64;
    let second = parse_u64(parts[2]).ok()? as i64;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some((hour, minute, second))
}

fn parse_i64(text: &str) -> Option<i64> {
    let (neg, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let v = parse_u64(digits).ok()? as i64;
    Some(if neg { -v } else { v })
}

/// Howard Hinnant's `days_from_civil`: (y, m, d) → days since 1970-01-01 for
/// the proleptic Gregorian calendar. Hand-rolled so the crate adds no date
/// crate (the parent verified nd-utils has no date helper).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as i64; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// `civil_from_days`: inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle time: `Sat, 06 Nov 2021 19:10:15 GMT` = unix 1636225815, pinned
    /// against Go's `http.ParseTime` output.
    const MTIME: i64 = 1_636_225_815;

    /// Oracle header value for MTIME, produced by Go's `TimeFormat`.
    const MTIME_HTTP: &str = "Sat, 06 Nov 2021 19:10:15 GMT";

    /// Direct-file metadata with a known mtime and no validator — exactly
    /// Navidrome's seekable case (`media_streamer.go:161` sets no ETag).
    fn meta(size: u64) -> FileMeta {
        FileMeta {
            size,
            last_modified_secs: Some(MTIME),
            etag: None,
            content_type: Some("audio/mpeg".to_string()),
            content_encoding: None,
        }
    }

    fn meta_etag(size: u64, etag: &str) -> FileMeta {
        let mut m = meta(size);
        m.etag = Some(etag.to_string());
        m
    }

    fn get(range: Option<&str>) -> CondRequest {
        CondRequest {
            method: "GET".to_string(),
            range: range.map(str::to_string),
            ..CondRequest::default()
        }
    }

    /// --- plain 200 -------------------------------------------------------

    #[test]
    fn plain_get_is_200_with_go_headers() {
        // Oracle row `plain  200 Content-Length=10 Accept-Ranges=bytes
        // Content-Type=audio/mpeg Last-Modified=Sat, 06 Nov 2021 19:10:15 GMT
        // Etag=-`
        let p = plan(&meta(10), &get(None));
        assert_eq!(p.status, 200);
        assert_eq!(p.start, 0);
        assert_eq!(p.end, 9);
        assert_eq!(p.content_length, Some(10));
        assert_eq!(p.bytes_to_write(), 10);
        assert_eq!(p.accept_ranges.as_deref(), Some("bytes"));
        assert_eq!(p.content_range, None);
        assert_eq!(p.last_modified.as_deref(), Some(MTIME_HTTP));
        assert_eq!(p.etag, None);
        assert_eq!(p.content_type.as_deref(), Some("audio/mpeg"));
        assert!(!p.no_body);
    }

    #[test]
    fn zero_size_plain_get_is_200_empty() {
        let p = plan(&meta(0), &get(None));
        assert_eq!(p.status, 200);
        assert_eq!(p.content_length, Some(0));
        assert_eq!(p.bytes_to_write(), 0);
        assert_eq!(p.accept_ranges.as_deref(), Some("bytes"));
    }

    #[test]
    fn accept_ranges_is_set_on_200_and_206() {
        // fs.go:398 sets it before the status is chosen.
        assert_eq!(
            plan(&meta(10), &get(None)).accept_ranges.as_deref(),
            Some("bytes")
        );
        assert_eq!(
            plan(&meta(10), &get(Some("bytes=2-4")))
                .accept_ranges
                .as_deref(),
            Some("bytes")
        );
    }

    #[test]
    fn content_encoding_suppresses_content_length_on_200_only() {
        // fs.go:426: skip Content-Length on a plain 200 when Content-Encoding
        // is set; a 206 always sets it.
        let mut m = meta(10);
        m.content_encoding = Some("gzip".to_string());
        assert_eq!(plan(&m, &get(None)).content_length, None);
        assert_eq!(
            plan(&m, &get(Some("bytes=2-4"))).content_length,
            Some(3),
            "a 206 must still declare its length"
        );
    }

    /// --- single-range 206 -------------------------------------------------

    #[test]
    fn single_range_headers_match_oracle() {
        let cases = [
            // (spec, start, end, length, content-range) — oracle rows.
            ("bytes=5-7", 5, 7, 3, "bytes 5-7/10"),
            ("bytes=5-", 5, 9, 5, "bytes 5-9/10"),
            ("bytes=-3", 7, 9, 3, "bytes 7-9/10"),
            ("bytes=-99", 0, 9, 10, "bytes 0-9/10"),
            ("bytes=0-0", 0, 0, 1, "bytes 0-0/10"),
            ("bytes=9-9", 9, 9, 1, "bytes 9-9/10"),
            ("bytes=2-9999", 2, 9, 8, "bytes 2-9/10"),
        ];
        for (spec, start, end, len, cr) in cases {
            let p = plan(&meta(10), &get(Some(spec)));
            assert_eq!(p.status, 206, "{spec}");
            assert_eq!(p.start, start, "{spec}");
            assert_eq!(p.end, end, "{spec}");
            assert_eq!(p.content_length, Some(len), "{spec}");
            assert_eq!(p.bytes_to_write(), len, "{spec}");
            assert_eq!(p.content_range.as_deref(), Some(cr), "{spec}");
            assert_eq!(p.accept_ranges.as_deref(), Some("bytes"));
        }
    }

    #[test]
    fn zero_length_suffix_is_416_deviation_from_go() {
        // Oracle: Go answers 206 with `Content-Range: bytes 10-9/10`, an
        // end-before-start spec. Deviation 3 turns it into a 416.
        let p = plan(&meta(10), &get(Some("bytes=-0")));
        assert_eq!(p.status, 416);
        assert_eq!(p.content_range.as_deref(), Some("bytes */10"));
        assert_eq!(p.bytes_to_write(), 0);
        assert!(p.no_body);
        assert_eq!(p.last_modified, None, "serveError clears Last-Modified");
        assert_eq!(p.accept_ranges, None);
    }

    /// --- unsatisfiable (well-formed) --------------------------------------

    #[test]
    fn beyond_eof_is_416_with_content_range_and_cleared_headers() {
        // Oracle rows `range-beyond`/`range-beyond-open`: 416,
        // Content-Range=bytes */10, no Accept-Ranges, no Last-Modified, no
        // Etag, no Content-Length.
        for spec in ["bytes=10-20", "bytes=10-", "bytes=11-"] {
            let p = plan(&meta_etag(10, "\"abc\""), &get(Some(spec)));
            assert_eq!(p.status, 416, "{spec}");
            assert_eq!(p.content_range.as_deref(), Some("bytes */10"));
            assert_eq!(p.content_length, None);
            assert_eq!(p.bytes_to_write(), 0);
            assert_eq!(p.accept_ranges, None);
            assert_eq!(p.last_modified, None);
            assert_eq!(p.etag, None, "serveError clears Etag (fs.go:200)");
            assert!(p.no_body);
            assert_eq!(
                p.error_message.as_deref(),
                Some("invalid range: failed to overlap")
            );
        }
    }

    #[test]
    fn empty_file_with_unsatisfiable_range_is_200_not_416() {
        // fs.go:325-332 carve-out; oracle rows `empty-file-with-range` and
        // `empty-file-with-range-beyond` both 200 Content-Length=0.
        for spec in ["bytes=0-10", "bytes=5-10"] {
            let p = plan(&meta(0), &get(Some(spec)));
            assert_eq!(p.status, 200, "{spec} must be a 200 on empty content");
            assert_eq!(p.content_length, Some(0));
            assert_eq!(p.content_range, None);
            assert_eq!(p.accept_ranges.as_deref(), Some("bytes"));
        }
    }

    #[test]
    fn no_overlap_is_reported_by_parse_range() {
        assert_eq!(
            parse_range(Some("bytes=10-20"), 10),
            RangeOutcome::NoOverlap
        );
        assert_eq!(parse_range(Some("bytes=10-"), 10), RangeOutcome::NoOverlap);
        // A mix of overlapping and non-overlapping items is satisfied: Go only
        // errors when NO range survived (fs.go:1116-1119).
        assert_eq!(
            parse_range(Some("bytes=10-20,2-4"), 10),
            RangeOutcome::Satisfied(vec![ByteRange { start: 2, end: 4 }])
        );
    }

    /// --- malformed / ignored ---------------------------------------------

    #[test]
    fn malformed_ranges_are_ignored_not_416() {
        // Deviation 2. Oracle shows Go's statuses for these same inputs:
        //   items=0-2, bytes=abc-def, bytes=5, bytes=-1-2, "  bytes=0-2",
        //   bytes=7-5 → 416 in Go; the port answers 200 per RFC 9110 §14.2.
        for spec in [
            "items=0-2",
            "bytes=abc-def",
            "bytes=5",
            "bytes=-1-2",
            "  bytes=0-2",
            "bytes=7-5",
            "bytes=1-2-",
            "bytes=0x1-0x2",
            "BYTES=0-2",
        ] {
            let p = plan(&meta(10), &get(Some(spec)));
            assert_eq!(p.status, 200, "{spec} must be ignored, got {}", p.status);
            assert_eq!(p.content_length, Some(10));
            assert_eq!(p.content_range, None);
            assert_eq!(p.bytes_to_write(), 10);
        }
    }

    #[test]
    fn empty_range_items_are_skipped_like_go() {
        // fs.go:1062-1064 `continue`. Oracle rows `range-empty-spec`
        // (`bytes=`) and `range-blank-items` (`bytes= , `) → 200 full content.
        for spec in ["bytes=", "bytes=,", "bytes= , "] {
            let p = plan(&meta(10), &get(Some(spec)));
            assert_eq!(p.status, 200, "{spec}");
            assert_eq!(p.bytes_to_write(), 10);
        }
    }

    #[test]
    fn trailing_comma_after_a_valid_range_is_206_like_go() {
        // Oracle row `range-trailing-comma-valid`: 206 `bytes 2-4/10`, because
        // Go skips the empty item (fs.go:1062-1064) and keeps the valid range.
        // The port does the same, so this is parity rather than a deviation.
        assert_eq!(
            parse_range(Some("bytes=2-4,"), 10),
            RangeOutcome::Satisfied(vec![ByteRange { start: 2, end: 4 }])
        );
        let p = plan(&meta(10), &get(Some("bytes=2-4,")));
        assert_eq!(p.status, 206);
        assert_eq!(p.content_range.as_deref(), Some("bytes 2-4/10"));
        assert_eq!(p.bytes_to_write(), 3);
    }

    #[test]
    fn whitespace_around_a_range_item_is_valid() {
        // Oracle rows `range-space-start`, `range-space-end`, `range-tab`: Go
        // trims the item and both cut halves (fs.go:1061, :1072), so all three
        // are 206 `bytes 2-4/10`. The port matches.
        for spec in ["bytes= 2-4", "bytes=2 -4", "bytes=\t2-4", "bytes=  2-4  "] {
            assert_eq!(
                parse_range(Some(spec), 10),
                RangeOutcome::Satisfied(vec![ByteRange { start: 2, end: 4 }]),
                "{spec} must be a valid single range"
            );
        }
        // Whitespace *inside* a number fails ParseInt. Oracle row
        // `range-space-in-num`: Go answers 416; the port ignores it (deviation 2).
        assert_eq!(parse_range(Some("bytes=1 2-3"), 10), RangeOutcome::Ignored);
        assert_eq!(plan(&meta(10), &get(Some("bytes=1 2-3"))).status, 200);
    }

    #[test]
    fn more_than_max_content_ranges_is_ignored() {
        // fs.go:1053-1057: numRanges counts commas, including a trailing one.
        let at_limit = format!("bytes={}0-1", "0-1,".repeat(MAX_CONTENT_RANGES - 1));
        assert!(at_limit.starts_with("bytes=0-1,0-1"));
        // exactly 200 ranges → processed (oracle `range-200` → 200 via the
        // summed-size guard, since 200×2 > 10)
        assert_eq!(
            parse_range(Some(at_limit.as_str()), 10),
            RangeOutcome::Ignored
        );
        let over = format!("bytes={}0-1", "0-1,".repeat(MAX_CONTENT_RANGES));
        assert_eq!(parse_range(Some(over.as_str()), 10), RangeOutcome::Ignored);
        // With a big file, 200 ranges stay satisfied: sum 400 <= size.
        let many = format!("bytes={}0-1", "0-1,".repeat(MAX_CONTENT_RANGES - 1));
        let r = parse_range(Some(many.as_str()), 10_000);
        assert!(matches!(r, RangeOutcome::Satisfied(v) if v.len() == MAX_CONTENT_RANGES));
        // 201 ranges → ignored outright even though the sum would fit.
        let too_many = format!("bytes={}0-1", "0-1,".repeat(MAX_CONTENT_RANGES));
        assert_eq!(
            parse_range(Some(too_many.as_str()), 10_000),
            RangeOutcome::Ignored
        );
    }

    #[test]
    fn summed_range_size_over_content_is_ignored() {
        // fs.go:341-346. Oracle row `range-multi-sum-over-size` → 200.
        assert_eq!(
            parse_range(Some("bytes=0-5,4-9"), 10),
            RangeOutcome::Ignored
        );
        let p = plan(&meta(10), &get(Some("bytes=0-5,4-9")));
        assert_eq!(p.status, 200);
        assert_eq!(p.bytes_to_write(), 10);
        // Overlapping but summing to exactly size is NOT over the size.
        assert_eq!(
            parse_range(Some("bytes=0-4,5-9"), 10),
            RangeOutcome::Satisfied(vec![
                ByteRange { start: 0, end: 4 },
                ByteRange { start: 5, end: 9 }
            ])
        );
    }

    #[test]
    fn multi_range_serves_full_content_as_documented_deviation() {
        // Deviation 1. Oracle row `range-multi`: Go answers 206 with
        // `Content-Type: multipart/byteranges; boundary=…` and
        // Content-Length=318 for a 10-byte body. The port answers plain 200.
        let p = plan(&meta(10), &get(Some("bytes=0-2,5-7")));
        assert_eq!(p.status, 200);
        assert_eq!(p.start, 0);
        assert_eq!(p.end, 9);
        assert_eq!(p.bytes_to_write(), 10);
        assert_eq!(p.content_range, None);
        assert_eq!(p.accept_ranges.as_deref(), Some("bytes"));
        assert_eq!(
            parse_range(Some("bytes=0-2,5-7"), 10),
            RangeOutcome::Satisfied(vec![
                ByteRange { start: 0, end: 2 },
                ByteRange { start: 5, end: 7 }
            ])
        );
    }

    #[test]
    fn range_absent_is_absent_not_ignored() {
        assert_eq!(parse_range(None, 10), RangeOutcome::Absent);
        assert_eq!(parse_range(Some(""), 10), RangeOutcome::Absent);
    }

    /// --- HEAD -------------------------------------------------------------

    #[test]
    fn head_writes_headers_and_no_body() {
        // Oracle rows `head` (200, Content-Length=10) and `head-range`
        // (206, Content-Length=3, Content-Range=bytes 2-4/10): Go declares the
        // length but skips the copy (fs.go:429-431).
        let mut req = get(None);
        req.method = "HEAD".to_string();
        let p = plan(&meta(10), &req);
        assert_eq!(p.status, 200);
        assert_eq!(p.content_length, Some(10), "HEAD still declares length");
        assert!(p.no_body);
        assert_eq!(p.bytes_to_write(), 0);

        let mut req2 = get(Some("bytes=2-4"));
        req2.method = "HEAD".to_string();
        let p2 = plan(&meta(10), &req2);
        assert_eq!(p2.status, 206);
        assert_eq!(p2.content_range.as_deref(), Some("bytes 2-4/10"));
        assert!(p2.no_body);
        assert_eq!(p2.bytes_to_write(), 0);
    }

    #[test]
    fn method_matching_is_case_insensitive_but_lowercase_works() {
        let mut req = get(None);
        req.method = "head".to_string();
        assert!(plan(&meta(10), &req).no_body);
        let mut post = CondRequest {
            method: "post".to_string(),
            ..CondRequest::default()
        };
        post.if_modified_since = Some(MTIME_HTTP.to_string());
        // Oracle row `ims-post-method`: 200, because IMS is GET/HEAD-only.
        assert_eq!(plan(&meta(10), &post).status, 200);
    }

    /// --- If-Match ---------------------------------------------------------

    #[test]
    fn if_match_hit_and_miss() {
        let hit = {
            let mut r = get(None);
            r.if_match = Some("\"abc\"".to_string());
            plan(&meta_etag(10, "\"abc\""), &r)
        };
        assert_eq!(hit.status, 200);
        assert_eq!(hit.bytes_to_write(), 10);

        let miss = {
            let mut r = get(None);
            r.if_match = Some("\"xyz\"".to_string());
            plan(&meta_etag(10, "\"abc\""), &r)
        };
        // Oracle row `if-match-miss`: 412, Last-Modified and Etag SURVIVE
        // (no serveError on this path), no Content-Length/Accept-Ranges.
        assert_eq!(miss.status, 412);
        assert_eq!(miss.bytes_to_write(), 0);
        assert_eq!(miss.content_length, None);
        assert_eq!(miss.accept_ranges, None);
        assert_eq!(miss.last_modified.as_deref(), Some(MTIME_HTTP));
        assert_eq!(miss.etag.as_deref(), Some("\"abc\""));
        assert_eq!(miss.error_message, None, "Go's 412 has no message");
    }

    #[test]
    fn if_match_without_server_etag_is_412() {
        // etagStrongMatch requires a[0] == '"' on BOTH sides (fs.go:463-466).
        // Oracle row `if-match-no-server-etag`: 412.
        let mut r = get(None);
        r.if_match = Some("\"abc\"".to_string());
        let p = plan(&meta(10), &r);
        assert_eq!(p.status, 412);
    }

    #[test]
    fn if_match_star_matches_without_a_server_etag() {
        // Oracle row `if-match-star` (with Etag) → 200; `*` is unconditional
        // in checkIfMatch (fs.go:497-499).
        let mut r = get(Some("bytes=0-2"));
        r.if_match = Some("*".to_string());
        assert_eq!(plan(&meta(10), &r).status, 206);
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &r).status, 206);
    }

    #[test]
    fn if_match_weak_server_tag_never_matches_strong_request() {
        // Oracle row `if-match-weak-server`: 412.
        let mut r = get(None);
        r.if_match = Some("\"abc\"".to_string());
        assert_eq!(plan(&meta_etag(10, "W/\"abc\""), &r).status, 412);
    }

    #[test]
    fn if_match_unquoted_token_never_matches() {
        // scanETag returns "" for `abc`, so the loop breaks → condFalse.
        // Oracle row `if-match-invalid-token`: 412.
        let mut r = get(None);
        r.if_match = Some("abc".to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &r).status, 412);
    }

    #[test]
    fn if_match_list_matches_any_member() {
        let mut r = get(None);
        r.if_match = Some("\"x\", \"abc\"".to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &r).status, 200);
        let mut r2 = get(None);
        r2.if_match = Some("\"x\", \"y\"".to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &r2).status, 412);
        // Leading commas are skipped (fs.go:492-495).
        let mut r3 = get(None);
        r3.if_match = Some(", , \"abc\"".to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &r3).status, 200);
    }

    #[test]
    fn if_match_wins_over_range_and_over_if_unmodified_since() {
        // Oracle rows `if-match-plus-range` (412) and `if-match-wins-over-iunmod`
        // (200, because If-Unmodified-Since is never consulted).
        let mut r = get(Some("bytes=0-2"));
        r.if_match = Some("\"xyz\"".to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &r).status, 412);

        let mut ok = get(None);
        ok.if_match = Some("\"abc\"".to_string());
        ok.if_unmodified_since = Some("Sat, 06 Nov 2021 19:00:00 GMT".to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &ok).status, 200);
    }

    /// --- If-Unmodified-Since ---------------------------------------------

    #[test]
    fn if_unmodified_since_matrix() {
        // Oracle rows: older-than-mtime → 412; equal → 200; bad date → 200;
        // zero mtime → 200 (header inert).
        let mut older = get(None);
        older.if_unmodified_since = Some("Sat, 06 Nov 2021 19:10:00 GMT".to_string());
        assert_eq!(plan(&meta(10), &older).status, 412);

        let mut equal = get(None);
        equal.if_unmodified_since = Some(MTIME_HTTP.to_string());
        assert_eq!(plan(&meta(10), &equal).status, 200);

        let mut newer = get(None);
        newer.if_unmodified_since = Some("Sat, 06 Nov 2021 19:10:16 GMT".to_string());
        assert_eq!(plan(&meta(10), &newer).status, 200);

        let mut garbage = get(None);
        garbage.if_unmodified_since = Some("garbage".to_string());
        assert_eq!(plan(&meta(10), &garbage).status, 200);

        let mut zero_mtime = get(None);
        zero_mtime.if_unmodified_since = Some("Sat, 06 Nov 2021 19:10:00 GMT".to_string());
        let m = FileMeta {
            size: 10,
            last_modified_secs: None,
            etag: None,
            content_type: Some("audio/mpeg".to_string()),
            content_encoding: None,
        };
        let p = plan(&m, &zero_mtime);
        assert_eq!(p.status, 200);
        assert_eq!(p.last_modified, None, "setLastModified skips zero time");
    }

    /// --- If-None-Match ----------------------------------------------------

    #[test]
    fn if_none_match_hit_is_304_and_wins_over_range() {
        let mut r = get(Some("bytes=0-2"));
        r.if_none_match = Some("\"abc\"".to_string());
        let p = plan(&meta_etag(10, "\"abc\""), &r);
        // Oracle row `if-none-match-hit`: 304, Content-Type/Length gone,
        // Last-Modified gone (ETag set), Etag kept.
        assert_eq!(p.status, 304);
        assert_eq!(p.content_length, None);
        assert_eq!(p.content_type, None);
        assert_eq!(
            p.last_modified, None,
            "writeNotModified deletes Last-Modified when Etag set"
        );
        assert_eq!(p.etag.as_deref(), Some("\"abc\""));
        assert_eq!(p.bytes_to_write(), 0);
        assert!(p.no_body);
    }

    #[test]
    fn if_none_match_without_server_etag_is_200() {
        // Oracle row `if-none-match-star` (no server Etag) is 304 because `*`
        // is unconditional; a concrete tag against no server tag is 200.
        let mut r = get(None);
        r.if_none_match = Some("\"abc\"".to_string());
        let p = plan(&meta(10), &r);
        assert_eq!(p.status, 200, "an unknown validator must not yield 304");
        assert_eq!(p.last_modified.as_deref(), Some(MTIME_HTTP));
    }

    #[test]
    fn if_none_match_star_is_304_even_with_no_server_etag() {
        // Oracle row `if-none-match-star`: 304, Last-Modified kept (no Etag).
        let mut r = get(None);
        r.if_none_match = Some("*".to_string());
        let p = plan(&meta(10), &r);
        assert_eq!(p.status, 304);
        assert_eq!(p.last_modified.as_deref(), Some(MTIME_HTTP));
        assert_eq!(p.etag, None);
    }

    #[test]
    fn if_none_match_uses_weak_comparison() {
        // Oracle row `if-none-match-weak-vs-strong`: W/"abc" vs "abc" → 304.
        let mut r = get(None);
        r.if_none_match = Some("W/\"abc\"".to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &r).status, 304);
        let mut r2 = get(None);
        r2.if_none_match = Some("\"abc\"".to_string());
        assert_eq!(plan(&meta_etag(10, "W/\"abc\""), &r2).status, 304);
    }

    #[test]
    fn if_none_match_on_post_is_412() {
        // fs.go:657-665; oracle row `if-none-match-post`: 412.
        let mut r = CondRequest {
            method: "POST".to_string(),
            ..CondRequest::default()
        };
        r.if_none_match = Some("\"abc\"".to_string());
        let p = plan(&meta_etag(10, "\"abc\""), &r);
        assert_eq!(p.status, 412);
        assert_eq!(p.last_modified.as_deref(), Some(MTIME_HTTP));
    }

    #[test]
    fn if_none_match_beats_if_modified_since() {
        // fs.go:656-668: If-Modified-Since is consulted only when
        // checkIfNoneMatch returned condNone. Oracle row `inm-beats-ims`: 200.
        let mut r = get(None);
        r.if_none_match = Some("\"nope\"".to_string());
        r.if_modified_since = Some(MTIME_HTTP.to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &r).status, 200);
    }

    /// --- If-Modified-Since -----------------------------------------------

    #[test]
    fn if_modified_since_matrix() {
        // Oracle rows: equal → 304; older → 200; newer → 304; bad date → 200;
        // zero mtime → 200; POST → 200.
        let mut equal = get(None);
        equal.if_modified_since = Some(MTIME_HTTP.to_string());
        assert_eq!(plan(&meta(10), &equal).status, 304);

        let mut older = get(None);
        older.if_modified_since = Some("Sat, 06 Nov 2021 19:10:14 GMT".to_string());
        assert_eq!(plan(&meta(10), &older).status, 200);

        let mut newer = get(None);
        newer.if_modified_since = Some("Sat, 06 Nov 2021 19:10:16 GMT".to_string());
        assert_eq!(plan(&meta(10), &newer).status, 304);

        let mut garbage = get(None);
        garbage.if_modified_since = Some("garbage".to_string());
        assert_eq!(plan(&meta(10), &garbage).status, 200);

        let m = FileMeta {
            size: 10,
            last_modified_secs: None,
            etag: None,
            content_type: Some("audio/mpeg".to_string()),
            content_encoding: None,
        };
        let mut zero = get(None);
        zero.if_modified_since = Some(MTIME_HTTP.to_string());
        assert_eq!(plan(&m, &zero).status, 200);

        let mut post = CondRequest {
            method: "POST".to_string(),
            ..CondRequest::default()
        };
        post.if_modified_since = Some(MTIME_HTTP.to_string());
        assert_eq!(plan(&meta(10), &post).status, 200);
    }

    #[test]
    fn if_modified_since_304_keeps_last_modified_when_no_etag() {
        // Oracle row `ims-equal`: 304 with Last-Modified kept, Etag=-.
        let mut r = get(None);
        r.if_modified_since = Some(MTIME_HTTP.to_string());
        let p = plan(&meta(10), &r);
        assert_eq!(p.status, 304);
        assert_eq!(p.last_modified.as_deref(), Some(MTIME_HTTP));
        assert_eq!(p.content_type, None);
        assert_eq!(p.content_length, None);
    }

    /// --- If-Range ---------------------------------------------------------

    #[test]
    fn if_range_etag_form() {
        let mut hit = get(Some("bytes=2-4"));
        hit.if_range = Some("\"abc\"".to_string());
        assert_eq!(plan(&meta_etag(10, "\"abc\""), &hit).status, 206);

        // Oracle row `if-range-etag-miss`: Range dropped → plain 200.
        let mut miss = get(Some("bytes=2-4"));
        miss.if_range = Some("\"zzz\"".to_string());
        let p = plan(&meta_etag(10, "\"abc\""), &miss);
        assert_eq!(p.status, 200);
        assert_eq!(p.bytes_to_write(), 10);
        assert_eq!(p.content_range, None);

        // Oracle row `if-range-etag-no-server`: no server Etag → strong match
        // fails → Range dropped → 200.
        let mut none = get(Some("bytes=2-4"));
        none.if_range = Some("\"abc\"".to_string());
        assert_eq!(plan(&meta(10), &none).status, 200);
    }

    #[test]
    fn if_range_date_form_requires_exact_seconds() {
        let mut hit = get(Some("bytes=2-4"));
        hit.if_range = Some(MTIME_HTTP.to_string());
        assert_eq!(plan(&meta(10), &hit).status, 206);

        // One second off is a miss — no truncation tolerance here (unlike IMS).
        let mut miss = get(Some("bytes=2-4"));
        miss.if_range = Some("Sat, 06 Nov 2021 19:10:16 GMT".to_string());
        assert_eq!(plan(&meta(10), &miss).status, 200);

        // Oracle row `if-range-date-zero-mtime`: condFalse → 200.
        let m = FileMeta {
            size: 10,
            last_modified_secs: None,
            etag: None,
            content_type: Some("audio/mpeg".to_string()),
            content_encoding: None,
        };
        assert_eq!(plan(&m, &hit).status, 200);

        // Oracle row `if-range-bad-value`: unparseable → condFalse → 200.
        let mut garbage = get(Some("bytes=2-4"));
        garbage.if_range = Some("garbage".to_string());
        assert_eq!(plan(&meta(10), &garbage).status, 200);
    }

    #[test]
    fn if_range_is_ignored_when_no_range_is_present() {
        // fs.go:669: `if rangeHeader != "" && checkIfRange(...)`. Oracle row
        // `if-range-without-range`: 200 full content.
        let mut r = get(None);
        r.if_range = Some("\"zzz\"".to_string());
        let p = plan(&meta_etag(10, "\"abc\""), &r);
        assert_eq!(p.status, 200);
        assert_eq!(p.bytes_to_write(), 10);
    }

    /// --- date parsing (the four→three layout correction) ------------------

    #[test]
    fn parse_http_time_accepts_the_three_go_layouts() {
        // Each pinned against Go's http.ParseTime output (unix=1636225815).
        assert_eq!(parse_http_time(MTIME_HTTP), Some(MTIME));
        assert_eq!(
            parse_http_time("Saturday, 06-Nov-21 19:10:15 GMT"),
            Some(MTIME),
            "RFC 850"
        );
        assert_eq!(
            parse_http_time("Saturday, 06-Nov-21 19:10:15 MST"),
            Some(MTIME),
            "RFC 850 with an unspecified zone"
        );
        assert_eq!(
            parse_http_time("Sat Nov  6 19:10:15 2021"),
            Some(MTIME),
            "ANSI C, space-padded day"
        );
        assert_eq!(
            parse_http_time("Sat Nov 06 19:10:15 2021"),
            Some(MTIME),
            "ANSI C, zero-padded day"
        );
        assert_eq!(
            parse_http_time("Tue Nov  6 19:10:15 2021"),
            Some(MTIME),
            "Go does not validate the weekday in ANSIC either"
        );
        assert_eq!(
            parse_http_time("Tue, 06 Nov 2021 19:10:15 GMT"),
            Some(MTIME),
            "Go does not validate the weekday"
        );
        // Oracle: http.ParseTime rejects `UTC` in the TimeFormat layout, and
        // ServeContent answers 200 for an IMS carrying it.
        assert_eq!(parse_http_time("Sun, 06 Nov 2021 19:10:15 UTC"), None);
        let mut utc_req = get(None);
        utc_req.if_modified_since = Some("Sun, 06 Nov 2021 19:10:15 UTC".to_string());
        assert_eq!(
            plan(&meta(10), &utc_req).status,
            200,
            "an unparseable IMS is inert, not a 304"
        );
        // RFC 850 date in If-Range is accepted (oracle `if-range-rfc850` → 206).
        let mut ir = get(Some("bytes=2-4"));
        ir.if_range = Some("Saturday, 06-Nov-21 19:10:15 GMT".to_string());
        assert_eq!(plan(&meta(10), &ir).status, 206);
    }

    #[test]
    fn parse_http_time_rejects_dashed_rfc1123() {
        // The parent's "four layouts" note is wrong for Go 1.27.3: timeFormats
        // is [TimeFormat, RFC850, ANSIC] (header.go:120-124). Verified failure:
        //   http.ParseTime("Sun, 06-Nov-2021 19:10:15 GMT") → error
        assert_eq!(parse_http_time("Sat, 06-Nov-2021 19:10:15 GMT"), None);
        assert_eq!(parse_http_time("Sat, 06 Nov 2021 19:10:15"), None);
        assert_eq!(parse_http_time("06 Nov 2021 19:10:15 GMT"), None);
        assert_eq!(parse_http_time(""), None);
        assert_eq!(parse_http_time("garbage"), None);
    }

    #[test]
    fn rfc850_century_rule_matches_go() {
        // time/format.go:1100-1105: two-digit year >= 69 → 1900s, else 2000s.
        // Oracle: 06-Nov-69 → -4769385; 06-Nov-70 → 26766615; 06-Nov-99 → 941915415.
        assert_eq!(
            parse_http_time("Sunday, 06-Nov-69 19:10:15 GMT"),
            Some(-4_769_385)
        );
        assert_eq!(
            parse_http_time("Sunday, 06-Nov-70 19:10:15 GMT"),
            Some(26_766_615)
        );
        assert_eq!(
            parse_http_time("Sunday, 06-Nov-99 19:10:15 GMT"),
            Some(941_915_415)
        );
        // 06-Nov-05 → 2005-11-06 19:10:15 UTC = 1131304215.
        assert_eq!(
            parse_http_time("Sunday, 06-Nov-05 19:10:15 GMT"),
            Some(1_131_304_215)
        );
        assert_eq!(parse_http_time("Thursday, 01-Jan-70 00:00:00 GMT"), Some(0));
    }

    #[test]
    fn http_date_round_trips_through_parse() {
        for secs in [0i64, 1, MTIME, 1_648_890_000, -1, -4_769_385, 2_147_483_647] {
            let text = http_date(secs);
            assert_eq!(
                parse_http_time(text.as_str()),
                Some(secs),
                "round trip failed for {secs}: {text}"
            );
        }
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(-1), "Wed, 31 Dec 1969 23:59:59 GMT");
    }

    /// --- non-seekable (transcoded) plan -----------------------------------

    #[test]
    fn streaming_plan_has_no_length_and_accepts_none() {
        // media_streamer.go:165: Accept-Ranges: none. Oracle-inapplicable
        // (ServeContent is not used on this path), so pinned from Go source.
        let p = plan_streaming(&meta(0), &get(Some("bytes=0-2")), None);
        assert_eq!(p.status, 200);
        assert_eq!(p.accept_ranges.as_deref(), Some("none"));
        assert_eq!(p.content_length, None, "a pipe has no length");
        assert_eq!(p.content_range, None);
        assert!(!p.no_body);
    }

    #[test]
    fn streaming_plan_advertises_estimated_length() {
        // media_streamer.go:168-172 (estimateContentLength param).
        let p = plan_streaming(&meta(0), &get(None), Some(12_345));
        assert_eq!(p.content_length, Some(12_345));
        assert_eq!(p.bytes_to_write(), 12_345);
    }

    #[test]
    fn streaming_plan_head_declares_nothing_and_writes_nothing() {
        // media_streamer.go:174-177: HEAD drains ffmpeg, writes no bytes.
        let mut req = get(None);
        req.method = "HEAD".to_string();
        let p = plan_streaming(&meta(0), &req, Some(500));
        assert_eq!(p.status, 200);
        assert!(p.no_body);
        assert_eq!(p.bytes_to_write(), 0);
        assert_eq!(p.content_length, None);
    }

    #[test]
    fn streaming_plan_conditionals() {
        let mut revalidate = get(None);
        revalidate.if_none_match = Some("\"abc\"".to_string());
        let p = plan_streaming(&meta_etag(0, "\"abc\""), &revalidate, None);
        assert_eq!(p.status, 304);
        assert_eq!(p.last_modified, None);

        let mut precondition = get(None);
        precondition.if_match = Some("\"bad\"".to_string());
        assert_eq!(
            plan_streaming(&meta_etag(0, "\"abc\""), &precondition, None).status,
            412
        );
    }

    /// --- helper units -----------------------------------------------------

    #[test]
    fn scan_etag_matches_go() {
        assert_eq!(scan_etag("\"abc\""), Some(("\"abc\"", "")));
        assert_eq!(scan_etag("W/\"abc\""), Some(("W/\"abc\"", "")));
        assert_eq!(scan_etag("\"abc\", \"d\""), Some(("\"abc\"", ", \"d\"")));
        assert_eq!(scan_etag("abc"), None);
        // `len(s[start:]) < 2` passes for `""`, the loop finds the closing
        // quote at i=1, and Go returns `(""", "")`. It is a *valid* empty tag
        // syntactically, but etagStrongMatch still rejects it (`a != ""` is on
        // the whole tag, so `""` passes that check while `a[0] == '"'` also
        // holds — meaning Go treats `""` as a strong match against a server
        // tag of `""` only).
        assert_eq!(
            scan_etag("\"\""),
            Some(("\"\"", "")),
            "Go accepts a syntactically empty tag"
        );
        assert_eq!(scan_etag("\""), None, "no closing quote");
        assert_eq!(scan_etag(""), None);
        assert_eq!(scan_etag("\"a b\""), None, "spaces are not tag characters");
        assert_eq!(scan_etag("\"a\tb\""), None, "tabs are not tag characters");
    }

    #[test]
    fn strong_and_weak_match_helpers() {
        assert!(etag_strong_match("\"a\"", "\"a\""));
        assert!(!etag_strong_match("", ""));
        assert!(!etag_strong_match("W/\"a\"", "W/\"a\""));
        assert!(!etag_strong_match("\"a\"", "W/\"a\""));
        assert!(etag_weak_match("W/\"a\"", "\"a\""));
        assert!(etag_weak_match("\"a\"", "\"a\""));
        assert!(!etag_weak_match("\"a\"", "\"b\""));
    }

    #[test]
    fn trim_http_ws_only_cuts_space_and_tab() {
        assert_eq!(trim_http_ws("  x\t"), "x");
        assert_eq!(trim_http_ws("\nx\n"), "\nx\n");
        assert_eq!(trim_http_ws(" x"), " x");
    }

    #[test]
    fn byte_range_length_is_inclusive() {
        assert_eq!(ByteRange { start: 0, end: 0 }.length(), 1);
        assert_eq!(ByteRange { start: 5, end: 9 }.length(), 5);
    }

    #[test]
    fn civil_round_trip_over_many_dates() {
        for days in [-40_000i64, -1, 0, 1, 365, 366, 60_000, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "days={days} → {y}-{m}-{d}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(60), (1970, 3, 2));
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1860, 6, 26), -40_000);
    }
}
