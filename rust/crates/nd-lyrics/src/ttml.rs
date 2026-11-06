//! #09 — port of `model/lyrics_ttml.go`: TTML (Timed Text Markup Language)
//! parsing, including word-level span timing, background (x-bg) vocal
//! layers, named agents, and iTunes translation/transliteration metadata
//! tracks.

use crate::model::{normalize_lyric_lang, Agent, Cue, Line, LyricList, Lyrics};
use crate::normalize::{normalize_line_timing, normalize_lyrics};
use nd_utils::str::sanitize_text;
use quick_xml::events::Event;
use quick_xml::Reader;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

/// `defaultTTMLFrameRate`.
const DEFAULT_TTML_FRAME_RATE: f64 = 30.0;
/// `defaultTTMLSubFrameRate`.
const DEFAULT_TTML_SUBFRAME_RATE: f64 = 1.0;
/// `defaultTTMLTickRate`.
const DEFAULT_TTML_TICK_RATE: f64 = 1.0;

/// `ttmlBackgroundAgentPrefix`.
pub(crate) const TTML_BACKGROUND_AGENT_PREFIX: &str = "__nd_bg__|";

fn offset_time_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([0-9]+(?:\.[0-9]+)?)(h|m|s|ms|f|t)$").unwrap())
}

fn xml_encoding_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)<\?xml([^>]*?)encoding\s*=\s*["'][^"']+["']([^>]*)\?>"#).unwrap()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TtmlTimeKind {
    Absolute,
    Offset,
    Ambiguous,
}

#[derive(Debug, Clone, Copy)]
struct TtmlTimingParams {
    frame_rate: f64,
    sub_frame_rate: f64,
    tick_rate: f64,
}

#[derive(Debug, Clone, Default)]
struct TtmlTimingContext {
    lang: String,
    role: String,
    agent_id: String,
    begin: i64,
    has_begin: bool,
    end: i64,
    has_end: bool,
    invalid: bool,
}

#[derive(Debug, Clone)]
struct TtmlLineRef {
    order: usize,
    line: Line,
}

#[derive(Debug, Clone)]
struct TtmlMetadataEntry {
    key: String,
    line: Line,
    seq: usize,
}

#[derive(Debug, Clone)]
struct TtmlResolvedMetadataLine {
    order: usize,
    seq: usize,
    line: Line,
}

#[derive(Debug, Clone, Default)]
struct TtmlDefinedAgent {
    // id lives as the map key in defined_agents; the port drops the field.
    r#type: String,
    name: String,
}

#[derive(Debug, Clone, Default)]
struct TtmlPiece {
    raw: String,
    cue: Option<Cue>,
    is_break: bool,
}

// quick-xml 0.37 bytes-API shims (the Go original is encoding/xml; the str
// migration in newer quick-xml is not used here).
fn qname_eq(name: quick_xml::name::QName<'_>, expected: &[u8]) -> bool {
    name.as_ref().eq_ignore_ascii_case(expected)
}

/// `isTTMLDocument`: cheap probe that stops at the first element and checks
/// whether it is `<tt>`.
fn is_ttml_document(contents: &[u8]) -> bool {
    let mut reader = Reader::from_reader(contents);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                return qname_eq(e.name(), b"tt");
            }
            Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => {}
        }
        buf.clear();
    }
}

/// `parseTTML`: parses a TTML document into its main/translation/
/// pronunciation tracks. Returns `None` when the content is not a TTML
/// document.
pub fn parse_ttml(default_lang: &str, contents: &[u8]) -> Result<Option<LyricList>, String> {
    let contents = xml_encoding_regex()
        .replace_all(
            &String::from_utf8_lossy(contents),
            r#"<?xml$1encoding="UTF-8"$2?>"#,
        )
        .to_string();

    // Skip non-TTML content so sniffing doesn't run the full TTML parse on
    // plain text.
    if !is_ttml_document(contents.as_bytes()) {
        return Ok(None);
    }

    let mut p = TtmlParser {
        params: TtmlTimingParams {
            frame_rate: DEFAULT_TTML_FRAME_RATE,
            sub_frame_rate: DEFAULT_TTML_SUBFRAME_RATE,
            tick_rate: DEFAULT_TTML_TICK_RATE,
        },
        main_lang_order: Vec::new(),
        main_lines_by_lang: HashMap::new(),
        main_line_refs_by_key: HashMap::new(),
        main_line_order: 0,
        translation_lang_order: Vec::new(),
        translation_entries_by_lg: HashMap::new(),
        pronunciation_lang_order: Vec::new(),
        pronunciation_entries_by_lg: HashMap::new(),
        defined_agents: HashMap::new(),
        metadata_seq: 0,
    };

    let root = TtmlTimingContext {
        lang: normalize_lyric_lang(default_lang),
        ..Default::default()
    };

    let mut reader = Reader::from_reader(contents.as_bytes());
    reader.config_mut().trim_text(false);
    parse_top_level(&mut p, &mut reader, root)?;

    Ok(Some(p.to_lyric_list()))
}

type Attrs = Vec<(String, String)>;

fn read_event(reader: &mut Reader<&[u8]>) -> Result<Event<'static>, String> {
    let mut buf = Vec::new();
    let ev = reader
        .read_event_into(&mut buf)
        .map_err(|e| format!("TTML XML error: {e}"))?;
    let owned = ev.into_owned();
    Ok(owned)
}

fn parse_top_level(
    p: &mut TtmlParser,
    reader: &mut Reader<&[u8]>,
    root: TtmlTimingContext,
) -> Result<(), String> {
    loop {
        match read_event(reader)? {
            Event::Start(start) => {
                let name = start.name().as_ref().to_vec();
                let attrs = collect_attrs(&start);
                let end_name = name.clone();
                parse_element(p, reader, &name, &attrs, &root)?;
                skip_to_end(reader, &end_name)?;
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Consumes events until the end tag `name` (assumed to follow a fully
/// consumed start) — used by `parse_top_level` after `parse_element` has
/// consumed its own subtree.
fn skip_to_end(reader: &mut Reader<&[u8]>, name: &[u8]) -> Result<(), String> {
    let mut depth = 0usize;
    loop {
        match read_event(reader)? {
            Event::Start(_) => depth += 1,
            Event::End(e) => {
                if depth == 0 && qname_eq(e.name(), name) {
                    return Ok(());
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

fn collect_attrs(e: &quick_xml::events::BytesStart<'_>) -> Attrs {
    let mut attrs = Vec::new();
    for a in e.attributes().flatten() {
        // Go's xml.Attr keeps Name.Local (prefix stripped): match `xml:lang`,
        // `ttm:agent`, `itunes:key`, `ttp:frameRate` etc. by local name.
        attrs.push((
            local_name(a.key.as_ref()),
            String::from_utf8_lossy(&a.value).into_owned(),
        ));
    }
    attrs
}

fn local_name(name: &[u8]) -> String {
    // Strip namespace prefix (Go xml keeps Name.Local).
    let s = String::from_utf8_lossy(name);
    match s.split_once(':') {
        Some((_, local)) => local.to_string(),
        None => s.into_owned(),
    }
}

fn attr_value<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim())
}

#[allow(dead_code)]
fn attr_or_empty(attrs: &[(String, String)], key: &str) -> String {
    attr_value(attrs, key).unwrap_or("").to_string()
}

struct TtmlParser {
    params: TtmlTimingParams,

    main_lang_order: Vec<String>,
    main_lines_by_lang: HashMap<String, Vec<Line>>,

    main_line_refs_by_key: HashMap<String, TtmlLineRef>,
    main_line_order: usize,

    translation_lang_order: Vec<String>,
    translation_entries_by_lg: HashMap<String, Vec<TtmlMetadataEntry>>,

    pronunciation_lang_order: Vec<String>,
    pronunciation_entries_by_lg: HashMap<String, Vec<TtmlMetadataEntry>>,

    defined_agents: HashMap<String, TtmlDefinedAgent>,

    metadata_seq: usize,
}

/// `parseElement` (Go): dispatches on the element name, handling metadata
/// tracks, agent definitions, `<p>` paragraphs, and container recursion. The
/// element's own end tag is consumed.
fn parse_element(
    p: &mut TtmlParser,
    reader: &mut Reader<&[u8]>,
    name: &[u8],
    attrs: &[(String, String)],
    parent: &TtmlTimingContext,
) -> Result<(), String> {
    let local = local_name(name).to_lowercase();
    if local == "tt" {
        p.update_timing_params(attrs);
    }

    match local.as_str() {
        "translation" => {
            return parse_metadata_track(
                p,
                reader,
                name,
                attrs,
                parent,
                crate::model::kind::TRANSLATION,
            )
        }
        "transliteration" => {
            return parse_metadata_track(
                p,
                reader,
                name,
                attrs,
                parent,
                crate::model::kind::PRONUNCIATION,
            )
        }
        "agent" => return parse_agent_definition(p, reader, name, attrs),
        _ => {}
    }

    let ctx = p.child_context(attrs, parent);
    if local == "p" {
        let (line_text, tokens) = parse_paragraph(p, reader, &ctx)?;
        if !ctx.invalid && !line_text.is_empty() {
            let mut parsed_line = Line {
                value: line_text,
                ..Default::default()
            };
            if ctx.has_begin {
                parsed_line.start = Some(ctx.begin);
            }
            if ctx.has_end {
                parsed_line.end = Some(ctx.end);
            }
            if !tokens.is_empty() {
                parsed_line.cue = tokens;
            }
            parsed_line = normalize_line_timing(parsed_line);

            let line_key = attr_value(attrs, "key").unwrap_or("").to_string();
            p.add_main_line(&ctx.lang, &line_key, parsed_line);
        }
        return Ok(());
    }

    let element_name = name.to_vec();
    loop {
        match read_event(reader)? {
            Event::Start(start) => {
                let child_name = start.name().as_ref().to_vec();
                let child_attrs = collect_attrs(&start);
                let child_local = local_name(&child_name).to_lowercase();

                // Optimization for `<span>`/other inline elements: consume via
                // parse_inline_element to keep cue handling consistent.
                if child_local == "span" || child_local == "br" {
                    // Reached only when the parent is not a <p> (that branch
                    // returns above); treat inline elements as plain recursion
                    // via their raw text.
                    let pieces = parse_inline_element(p, reader, &child_name, &child_attrs, &ctx)?;
                    let _ = pieces; // pieces only matter inside <p>/metadata <text>
                } else {
                    let next_parent = if ctx.invalid {
                        parent.clone()
                    } else {
                        ctx.clone()
                    };
                    parse_element(p, reader, &child_name, &child_attrs, &next_parent)?;
                }
            }
            Event::End(e) => {
                if e.name().as_ref().eq_ignore_ascii_case(&element_name) {
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// `parseMetadataTrack`.
fn parse_metadata_track(
    p: &mut TtmlParser,
    reader: &mut Reader<&[u8]>,
    name: &[u8],
    attrs: &[(String, String)],
    parent: &TtmlTimingContext,
    kind: &str,
) -> Result<(), String> {
    let ctx = p.child_context(attrs, parent);
    let lang = normalize_lyric_lang(&ctx.lang);

    let element_name = name.to_vec();
    loop {
        match read_event(reader)? {
            Event::Start(start) => {
                let child_name = start.name().as_ref().to_vec();
                let child_attrs = collect_attrs(&start);
                if local_name(&child_name).eq_ignore_ascii_case("text") {
                    if let Some(entry) =
                        parse_metadata_text(p, reader, &child_name, &child_attrs, &ctx)?
                    {
                        p.add_metadata_entry(kind, &lang, entry);
                    }
                    continue;
                }

                let next_parent = if ctx.invalid {
                    parent.clone()
                } else {
                    ctx.clone()
                };
                parse_element(p, reader, &child_name, &child_attrs, &next_parent)?;
            }
            Event::End(e) => {
                if e.name().as_ref().eq_ignore_ascii_case(&element_name) {
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// `parseAgentDefinition`.
fn parse_agent_definition(
    p: &mut TtmlParser,
    reader: &mut Reader<&[u8]>,
    name: &[u8],
    attrs: &[(String, String)],
) -> Result<(), String> {
    let Some(id) = attr_value(attrs, "id").map(str::to_owned) else {
        return skip_element(reader);
    };
    if id.is_empty() {
        return skip_element(reader);
    }

    let mut agent = TtmlDefinedAgent {
        r#type: attr_value(attrs, "type").unwrap_or("").to_lowercase(),
        name: String::new(),
    };

    let element_name = name.to_vec();
    loop {
        match read_event(reader)? {
            Event::Start(start) => {
                let child_name = start.name().as_ref().to_vec();
                if local_name(&child_name).eq_ignore_ascii_case("name") {
                    let name_text = collect_element_text(reader, &child_name)?;
                    let name_text = sanitize_ttml_text(&name_text);
                    if !name_text.is_empty() && agent.name.is_empty() {
                        agent.name = name_text;
                    }
                    continue;
                }
                skip_element_named(reader, &child_name)?;
            }
            Event::End(e) => {
                if e.name().as_ref().eq_ignore_ascii_case(&element_name) {
                    p.defined_agents.insert(id.clone(), agent.clone());
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// `parseMetadataText`.
fn parse_metadata_text(
    p: &mut TtmlParser,
    reader: &mut Reader<&[u8]>,
    name: &[u8],
    attrs: &[(String, String)],
    parent: &TtmlTimingContext,
) -> Result<Option<TtmlMetadataEntry>, String> {
    let Some(for_key) = attr_value(attrs, "for").map(str::to_owned) else {
        // No for attribute: consume and skip.
        let _ = parse_inline_element(p, reader, name, attrs, parent)?;
        return Ok(None);
    };
    if for_key.trim().is_empty() {
        let _ = parse_inline_element(p, reader, name, attrs, parent)?;
        return Ok(None);
    }

    let pieces = parse_inline_element(p, reader, name, attrs, parent)?;
    let ctx = p.child_context(attrs, parent);
    if ctx.invalid {
        return Ok(None);
    }

    let (value, tokens) = build_ttml_line_from_pieces(&pieces);
    let mut line = Line {
        value,
        ..Default::default()
    };
    if ctx.has_begin {
        line.start = Some(ctx.begin);
    }
    if ctx.has_end {
        line.end = Some(ctx.end);
    }
    if !tokens.is_empty() {
        line.cue = tokens;
    }
    line = normalize_line_timing(line);

    if line.value.is_empty() && line.cue.is_empty() {
        return Ok(None);
    }

    Ok(Some(TtmlMetadataEntry {
        key: for_key.trim().to_string(),
        line,
        seq: 0,
    }))
}

/// `parseParagraph`.
fn parse_paragraph(
    p: &TtmlParser,
    reader: &mut Reader<&[u8]>,
    ctx: &TtmlTimingContext,
) -> Result<(String, Vec<Cue>), String> {
    let mut pieces: Vec<TtmlPiece> = Vec::new();
    let p_ctx = ctx.clone();
    loop {
        match read_event(reader)? {
            Event::Start(start) => {
                let child_name = start.name().as_ref().to_vec();
                let child_attrs = collect_attrs(&start);
                let child_local = local_name(&child_name).to_lowercase();
                if child_local == "br" {
                    // <br></br>: consume its end if present.
                    consume_empty_element(reader, &child_name)?;
                    pieces.push(TtmlPiece {
                        is_break: true,
                        ..Default::default()
                    });
                    continue;
                }
                let inline = parse_inline_element_with(
                    reader,
                    &child_name,
                    &child_attrs,
                    &p_ctx,
                    Some(p),
                    &p.params,
                )?;
                pieces.extend(inline);
            }
            Event::Empty(start) => {
                // Self-closing elements (quick-xml Event::Empty): Go's decoder
                // sees `<br/>` as Start+End, i.e. a line break with no content.
                if local_name(start.name().as_ref()).to_lowercase() == "br" {
                    pieces.push(TtmlPiece {
                        is_break: true,
                        ..Default::default()
                    });
                }
            }
            Event::End(e) => {
                if qname_eq(e.name(), b"p") {
                    let (value, tokens) = build_ttml_line_from_pieces(&pieces);
                    return Ok((value, tokens));
                }
            }
            Event::Eof => return Ok((String::new(), Vec::new())),
            Event::Text(t) => {
                let text = String::from_utf8_lossy(t.as_ref()).into_owned();
                pieces.push(TtmlPiece {
                    raw: text,
                    ..Default::default()
                });
            }
            Event::CData(t) => {
                pieces.push(TtmlPiece {
                    raw: String::from_utf8_lossy(t.as_ref()).into_owned(),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
}

/// Consumes events for an element whose start we've seen and whose content we
/// don't care about, until (and including) its matching end tag. Handles both
/// `<br/>` (self-closing → possibly no end tag) and `<x></x>`.
fn consume_empty_element(reader: &mut Reader<&[u8]>, name: &[u8]) -> Result<(), String> {
    // Check whether the very next end event matches; self-closing elements
    // never emit Start, so read the next event.
    let mut buf = Vec::new();
    let ev = reader
        .read_event_into(&mut buf)
        .map_err(|e| e.to_string())?;
    match ev {
        Event::End(e) if qname_eq(e.name(), name) => Ok(()),
        _ => {
            // Unexpected content: skip to the end tag of `name`.
            let mut depth = 1usize;
            let mut buf = Vec::new();
            loop {
                let ev = reader
                    .read_event_into(&mut buf)
                    .map_err(|e| e.to_string())?;
                match ev {
                    Event::Start(_) => depth += 1,
                    Event::End(e) => {
                        if depth == 0 && qname_eq(e.name(), name) {
                            return Ok(());
                        }
                        depth = depth.saturating_sub(1);
                        if depth == 0 && qname_eq(e.name(), name) {
                            return Ok(());
                        }
                    }
                    Event::Eof => return Ok(()),
                    _ => {}
                }
                buf.clear();
            }
        }
    }
}

/// `parseInlineElement` — needs the parser (for agent resolution and params).
fn parse_inline_element(
    p: &TtmlParser,
    reader: &mut Reader<&[u8]>,
    name: &[u8],
    attrs: &[(String, String)],
    parent: &TtmlTimingContext,
) -> Result<Vec<TtmlPiece>, String> {
    parse_inline_element_with(reader, name, attrs, parent, Some(p), &p.params)
}

/// Core of `parseInlineElement` with explicit parser reference (or `None`
/// when called from contexts where the parser's own state is irrelevant —
/// not used in practice; the Go code always has `p`).
fn parse_inline_element_with(
    reader: &mut Reader<&[u8]>,
    name: &[u8],
    attrs: &[(String, String)],
    parent: &TtmlTimingContext,
    p: Option<&TtmlParser>,
    params: &TtmlTimingParams,
) -> Result<Vec<TtmlPiece>, String> {
    let local = local_name(name).to_lowercase();
    if local == "br" {
        consume_empty_element(reader, name)?;
        return Ok(vec![TtmlPiece {
            is_break: true,
            ..Default::default()
        }]);
    }

    // childContext needs params; reconstruct a minimal parser view.
    let ctx = child_context_with(attrs, parent, params);
    let has_own_timing = attr_value(attrs, "begin").is_some()
        || attr_value(attrs, "end").is_some()
        || attr_value(attrs, "dur").is_some();

    let mut pieces: Vec<TtmlPiece> = Vec::new();
    let element_name = name.to_vec();
    loop {
        match read_event(reader)? {
            Event::Start(start) => {
                let child_name = start.name().as_ref().to_vec();
                let child_attrs = collect_attrs(&start);
                let child_local = local_name(&child_name).to_lowercase();
                if child_local == "br" {
                    consume_empty_element(reader, &child_name)?;
                    pieces.push(TtmlPiece {
                        is_break: true,
                        ..Default::default()
                    });
                    continue;
                }
                let inline =
                    parse_inline_element_with(reader, &child_name, &child_attrs, &ctx, p, params)?;
                pieces.extend(inline);
            }
            Event::Empty(start) => {
                // Self-closing `<br/>`: Go's decoder yields Start+End for it.
                if local_name(start.name().as_ref()).to_lowercase() == "br" {
                    pieces.push(TtmlPiece {
                        is_break: true,
                        ..Default::default()
                    });
                }
            }
            Event::End(e) => {
                if !e.name().as_ref().eq_ignore_ascii_case(&element_name) {
                    continue;
                }

                if local == "span"
                    && has_own_timing
                    && !ctx.invalid
                    && !pieces.iter().any(|pc| pc.cue.is_some())
                {
                    let raw_value = concat_ttml_piece_raw(&pieces);
                    let token_text = sanitize_ttml_text(&raw_value);
                    if !token_text.is_empty() {
                        let mut parsed_token = Cue {
                            agent_id: p
                                .map(|pp| pp.resolve_cue_agent_id(&ctx))
                                .unwrap_or_default(),
                            ..Default::default()
                        };
                        if ctx.has_begin {
                            parsed_token.start = Some(ctx.begin);
                        }
                        if ctx.has_end {
                            parsed_token.end = Some(ctx.end);
                        }
                        return Ok(vec![TtmlPiece {
                            raw: raw_value,
                            cue: Some(parsed_token),
                            ..Default::default()
                        }]);
                    }
                }

                return Ok(pieces);
            }
            Event::Eof => return Ok(pieces),
            Event::Text(t) => {
                let text = String::from_utf8_lossy(t.as_ref()).into_owned();
                pieces.push(TtmlPiece {
                    raw: text,
                    ..Default::default()
                });
            }
            Event::CData(t) => {
                pieces.push(TtmlPiece {
                    raw: String::from_utf8_lossy(t.as_ref()).into_owned(),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
}

/// `buildTTMLLineFromPieces`.
fn build_ttml_line_from_pieces(pieces: &[TtmlPiece]) -> (String, Vec<Cue>) {
    let mut finalized = finalize_ttml_lines(split_ttml_pieces_by_break(pieces));
    while let Some(first) = finalized.first() {
        if first.text.is_empty() && first.cues.is_empty() {
            finalized.remove(0);
        } else {
            break;
        }
    }
    while let Some(last) = finalized.last() {
        if !last.text.is_empty() || !last.cues.is_empty() {
            break;
        }
        finalized.pop();
    }

    let mut value = String::new();
    let mut cues: Vec<Cue> = Vec::with_capacity(8);
    let mut byte_offset: usize = 0;
    for (i, line) in finalized.iter().enumerate() {
        if i > 0 {
            value.push('\n');
            byte_offset += 1;
        }
        value.push_str(&line.text);
        for cue in &line.cues {
            let mut cue = cue.clone();
            cue.byte_start += byte_offset;
            cue.byte_end += byte_offset;
            cues.push(cue);
        }
        byte_offset += line.text.len();
    }

    (value, cues)
}

#[derive(Default)]
struct TtmlFinalLine {
    text: String,
    cues: Vec<Cue>,
}

fn finalize_ttml_lines(lines: Vec<Vec<TtmlPiece>>) -> Vec<TtmlFinalLine> {
    lines
        .into_iter()
        .map(|line| {
            let (text, cues) = finalize_ttml_logical_line(&line);
            TtmlFinalLine { text, cues }
        })
        .collect()
}

fn split_ttml_pieces_by_break(pieces: &[TtmlPiece]) -> Vec<Vec<TtmlPiece>> {
    let mut lines: Vec<Vec<TtmlPiece>> = vec![Vec::new()];
    let mut prev_ended_with_space = true; // leading whitespace on a fresh line is dropped
    for piece in pieces {
        if piece.is_break {
            lines.push(Vec::new());
            prev_ended_with_space = true;
            continue;
        }

        let mut raw = normalize_ttml_piece_raw(&piece.raw);
        // Collapse whitespace across piece boundaries: a piece's leading
        // space is redundant when the text emitted so far already ends with
        // one.
        if prev_ended_with_space {
            raw = raw.strip_prefix(' ').map(str::to_string).unwrap_or(raw);
        }
        if raw.is_empty() {
            continue;
        }
        prev_ended_with_space = raw.ends_with(' ');
        lines.last_mut().unwrap().push(TtmlPiece {
            raw,
            cue: piece.cue.clone(),
            ..Default::default()
        });
    }
    lines
}

fn finalize_ttml_logical_line(line: &[TtmlPiece]) -> (String, Vec<Cue>) {
    let raw_line = concat_ttml_piece_raw(line);
    if raw_line.is_empty() {
        return (String::new(), Vec::new());
    }

    let left_trim_bytes = raw_line.len() - raw_line.trim_start().len();
    let right_trim_bytes = raw_line.len() - raw_line.trim_end().len();
    let mut trimmed_end = raw_line.len() - right_trim_bytes;
    if trimmed_end < left_trim_bytes {
        trimmed_end = left_trim_bytes;
    }

    let trimmed = raw_line.trim().to_string();
    let mut cues: Vec<Cue> = Vec::with_capacity(line.len());
    let mut cursor: usize = 0;
    for piece in line {
        let piece_end = cursor + piece.raw.len();
        if let Some(pc) = &piece.cue {
            let byte_start = cursor.max(left_trim_bytes);
            let byte_end = piece_end.min(trimmed_end);
            if byte_start < byte_end {
                let mut cue = pc.clone();
                cue.value = raw_line[byte_start..byte_end].to_string();
                cue.byte_start = byte_start - left_trim_bytes;
                cue.byte_end = byte_end - left_trim_bytes - 1;
                cues.push(cue);
            }
        }
        cursor = piece_end;
    }

    (trimmed, cues)
}

/// `normalizeTTMLPieceRaw`: collapses whitespace following TTML's default
/// mode (xml:space="default", the root default per TTML2 §8.1.1): per
/// §8.2.10 that means linefeed-treatment="treat-as-space" and
/// white-space-collapse="true", so linefeeds and other whitespace runs
/// collapse to a single space.
fn normalize_ttml_piece_raw(raw: &str) -> String {
    let raw = sanitize_text(raw);
    collapse_ttml_whitespace(&raw)
}

fn collapse_ttml_whitespace(raw: &str) -> String {
    let mut b = String::with_capacity(raw.len());
    let mut prev_space = false;
    for r in raw.chars() {
        // Only the XML S production (space, tab, CR, LF) is collapsible
        // whitespace. Other Unicode spaces (e.g. NBSP, U+3000) are content
        // characters, not whitespace, so they pass through unchanged.
        if matches!(r, ' ' | '\t' | '\n' | '\r') {
            if !prev_space {
                b.push(' ');
                prev_space = true;
            }
            continue;
        }
        b.push(r);
        prev_space = false;
    }
    b
}

fn concat_ttml_piece_raw(pieces: &[TtmlPiece]) -> String {
    let mut raw = String::new();
    for piece in pieces {
        raw.push_str(&normalize_ttml_piece_raw(&piece.raw));
    }
    raw
}

/// `collectElementText`.
fn collect_element_text(reader: &mut Reader<&[u8]>, name: &[u8]) -> Result<String, String> {
    let mut text = String::new();
    let element_name = name.to_vec();
    loop {
        match read_event(reader)? {
            Event::Start(start) => {
                let child_name = start.name().as_ref().to_vec();
                text.push_str(&collect_element_text(reader, &child_name)?);
            }
            Event::End(e) => {
                if e.name().as_ref().eq_ignore_ascii_case(&element_name) {
                    return Ok(text);
                }
            }
            Event::Eof => return Ok(text),
            Event::Text(t) => text.push_str(&String::from_utf8_lossy(t.as_ref())),
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(t.as_ref())),
            _ => {}
        }
    }
}

/// `skipElement`.
fn skip_element(reader: &mut Reader<&[u8]>) -> Result<(), String> {
    let mut depth = 1usize;
    let mut buf = Vec::new();
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| e.to_string())?;
        match ev {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
        buf.clear();
    }
}

fn skip_element_named(reader: &mut Reader<&[u8]>, name: &[u8]) -> Result<(), String> {
    let mut depth = 1usize;
    let mut buf = Vec::new();
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| e.to_string())?;
        match ev {
            Event::Start(_) => depth += 1,
            Event::End(e) => {
                if depth == 0 && qname_eq(e.name(), name) {
                    return Ok(());
                }
                depth = depth.saturating_sub(1);
                if depth == 0 && qname_eq(e.name(), name) {
                    return Ok(());
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
        buf.clear();
    }
}

impl TtmlParser {
    /// `toLyricList`.
    fn to_lyric_list(&self) -> LyricList {
        let mut res: LyricList = Vec::with_capacity(
            self.main_lang_order.len()
                + self.translation_lang_order.len()
                + self.pronunciation_lang_order.len(),
        );
        for lang in &self.main_lang_order {
            let lines = &self.main_lines_by_lang[lang];
            if lines.is_empty() {
                continue;
            }
            res.push(self.finalize_lyrics(Lyrics {
                kind: crate::model::kind::MAIN.to_string(),
                lang: lang.clone(),
                line: lines.clone(),
                synced: lines_are_synced(lines),
                ..Default::default()
            }));
        }

        res.extend(self.build_metadata_lyrics(
            crate::model::kind::TRANSLATION,
            &self.translation_lang_order,
            &self.translation_entries_by_lg,
        ));
        res.extend(self.build_metadata_lyrics(
            crate::model::kind::PRONUNCIATION,
            &self.pronunciation_lang_order,
            &self.pronunciation_entries_by_lg,
        ));
        res
    }

    /// `buildMetadataLyrics`.
    fn build_metadata_lyrics(
        &self,
        kind: &str,
        lang_order: &[String],
        entries_by_lang: &HashMap<String, Vec<TtmlMetadataEntry>>,
    ) -> LyricList {
        let mut res: LyricList = Vec::with_capacity(lang_order.len());

        for lang in lang_order {
            let Some(entries) = entries_by_lang.get(lang) else {
                continue;
            };
            if entries.is_empty() {
                continue;
            }

            let mut seen_keys: std::collections::HashSet<String> =
                std::collections::HashSet::with_capacity(entries.len());
            let mut resolved: Vec<TtmlResolvedMetadataLine> = Vec::with_capacity(entries.len());
            for entry in entries {
                if !seen_keys.insert(entry.key.clone()) {
                    continue;
                }

                let Some(line_ref) = self.main_line_refs_by_key.get(&entry.key) else {
                    // Go logs a warning here; skipping is the observable
                    // behavior.
                    continue;
                };

                let mut line = entry.line.clone();
                if line.start.is_none() && line_ref.line.start.is_some() {
                    line.start = line_ref.line.start;
                }
                if line.end.is_none() && line_ref.line.end.is_some() {
                    line.end = line_ref.line.end;
                }
                line = normalize_line_timing(line);

                if line.value.is_empty() && line.cue.is_empty() {
                    continue;
                }

                resolved.push(TtmlResolvedMetadataLine {
                    order: line_ref.order,
                    seq: entry.seq,
                    line,
                });
            }

            if resolved.is_empty() {
                continue;
            }

            resolved.sort_by_key(|r| (r.order, r.seq));

            let lines: Vec<Line> = resolved.into_iter().map(|r| r.line).collect();
            res.push(self.finalize_lyrics(Lyrics {
                kind: kind.to_string(),
                lang: lang.clone(),
                line: lines.clone(),
                synced: lines_are_synced(&lines),
                ..Default::default()
            }));
        }

        res
    }

    /// `finalizeLyrics`.
    fn finalize_lyrics(&self, mut lyrics: Lyrics) -> Lyrics {
        let (lines, agents) = self.resolve_agents(lyrics.line);
        lyrics.line = lines;
        lyrics.agents = agents;
        normalize_lyrics(lyrics)
    }

    /// `resolveAgents`.
    fn resolve_agents(&self, mut lines: Vec<Line>) -> (Vec<Line>, Vec<Agent>) {
        if lines.is_empty() {
            return (lines, Vec::new());
        }

        let mut used_order: Vec<String> = Vec::with_capacity(4);
        let mut used_set: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(4);
        let mut saw_empty_cue = false;

        for line in &lines {
            for cue in &line.cue {
                let agent_id = cue.agent_id.trim().to_string();
                if agent_id.is_empty() {
                    saw_empty_cue = true;
                    continue;
                }
                if used_set.insert(agent_id.clone()) {
                    used_order.push(agent_id);
                }
            }
        }

        if used_order.is_empty() {
            return (lines, Vec::new());
        }

        let mut main_id = String::new();
        for agent_id in &used_order {
            let role = self.base_role_for_agent(agent_id);
            if role != "bg" && role != "group" {
                main_id = agent_id.clone();
                break;
            }
        }
        if main_id.is_empty() && saw_empty_cue {
            main_id = "main".to_string();
        }
        if main_id.is_empty() {
            for agent_id in &used_order {
                if self.base_role_for_agent(agent_id) != "bg" {
                    main_id = agent_id.clone();
                    break;
                }
            }
        }
        if main_id.is_empty() {
            main_id = used_order[0].clone();
        }

        if used_set.insert(main_id.clone()) {
            used_order.insert(0, main_id.clone());
        }

        for line in &mut lines {
            for cue in &mut line.cue {
                if cue.agent_id.trim().is_empty() {
                    cue.agent_id = main_id.clone();
                }
            }
        }

        let mut agents: Vec<Agent> = Vec::with_capacity(used_order.len());
        for agent_id in &used_order {
            let mut role = self.base_role_for_agent(agent_id);
            if *agent_id == main_id {
                role = "main".to_string();
            }
            agents.push(Agent {
                id: agent_id.clone(),
                role,
                name: self.agent_name_for_id(agent_id),
            });
        }

        (lines, agents)
    }

    /// `resolveCueAgentID`.
    fn resolve_cue_agent_id(&self, ctx: &TtmlTimingContext) -> String {
        let mut agent_id = ctx.agent_id.trim().to_string();
        if context_has_role(&ctx.role, "x-bg") {
            if agent_id.is_empty() {
                agent_id = "main".to_string();
            }
            return background_agent_id(&agent_id);
        }
        agent_id
    }

    /// `baseRoleForAgent`.
    fn base_role_for_agent(&self, agent_id: &str) -> String {
        if is_background_agent_id(agent_id) {
            return "bg".to_string();
        }

        if let Some(agent) = self.defined_agents.get(agent_id) {
            return match agent.r#type.as_str() {
                "group" => "group".to_string(),
                _ => "voice".to_string(),
            };
        }

        "voice".to_string()
    }

    /// `agentNameForID`.
    fn agent_name_for_id(&self, agent_id: &str) -> String {
        if is_background_agent_id(agent_id) {
            let base_id = agent_id
                .strip_prefix(TTML_BACKGROUND_AGENT_PREFIX)
                .unwrap_or(agent_id);
            if base_id == "main" {
                return String::new();
            }
            if let Some(agent) = self.defined_agents.get(base_id) {
                return agent.name.clone();
            }
            return String::new();
        }

        if let Some(agent) = self.defined_agents.get(agent_id) {
            return agent.name.clone();
        }

        String::new()
    }

    /// `addMainLine`.
    fn add_main_line(&mut self, lang: &str, line_key: &str, line: Line) {
        let lang = normalize_lyric_lang(lang);
        if !self.main_lines_by_lang.contains_key(&lang) {
            self.main_lang_order.push(lang.clone());
        }
        self.main_lines_by_lang
            .entry(lang)
            .or_default()
            .push(line.clone());

        let line_key = line_key.trim();
        if !line_key.is_empty() {
            self.main_line_refs_by_key
                .entry(line_key.to_string())
                .or_insert(TtmlLineRef {
                    order: self.main_line_order,
                    line,
                });
        }
        self.main_line_order += 1;
    }

    /// `addMetadataEntry`.
    fn add_metadata_entry(&mut self, kind: &str, lang: &str, mut entry: TtmlMetadataEntry) {
        let lang = normalize_lyric_lang(lang);
        entry.seq = self.metadata_seq;
        self.metadata_seq += 1;

        match kind {
            crate::model::kind::TRANSLATION => {
                if !self.translation_entries_by_lg.contains_key(&lang) {
                    self.translation_lang_order.push(lang.clone());
                }
                self.translation_entries_by_lg
                    .entry(lang)
                    .or_default()
                    .push(entry);
            }
            crate::model::kind::PRONUNCIATION => {
                if !self.pronunciation_entries_by_lg.contains_key(&lang) {
                    self.pronunciation_lang_order.push(lang.clone());
                }
                self.pronunciation_entries_by_lg
                    .entry(lang)
                    .or_default()
                    .push(entry);
            }
            _ => {}
        }
    }

    /// `childContext`.
    fn child_context(
        &self,
        attrs: &[(String, String)],
        parent: &TtmlTimingContext,
    ) -> TtmlTimingContext {
        child_context_with(attrs, parent, &self.params)
    }

    /// `updateTimingParams`.
    fn update_timing_params(&mut self, attrs: &[(String, String)]) {
        let mut frame_rate = self.params.frame_rate;
        if let Some(value) = attr_value(attrs, "frameRate") {
            if let Ok(parsed) = value.parse::<f64>() {
                if parsed > 0.0 {
                    frame_rate = parsed;
                }
            }
        }

        if let Some(value) = attr_value(attrs, "frameRateMultiplier") {
            let parts: Vec<&str> = value.split_whitespace().collect();
            if parts.len() == 2 {
                if let (Ok(numerator), Ok(denominator)) =
                    (parts[0].parse::<f64>(), parts[1].parse::<f64>())
                {
                    if denominator > 0.0 {
                        frame_rate *= numerator / denominator;
                    }
                }
            }
        }

        let mut sub_frame_rate = self.params.sub_frame_rate;
        if let Some(value) = attr_value(attrs, "subFrameRate") {
            if let Ok(parsed) = value.parse::<f64>() {
                if parsed > 0.0 {
                    sub_frame_rate = parsed;
                }
            }
        }

        let mut tick_rate = self.params.tick_rate;
        if let Some(value) = attr_value(attrs, "tickRate") {
            if let Ok(parsed) = value.parse::<f64>() {
                if parsed > 0.0 {
                    tick_rate = parsed;
                }
            }
        }

        self.params.frame_rate = if frame_rate > 0.0 {
            frame_rate
        } else {
            DEFAULT_TTML_FRAME_RATE
        };
        self.params.sub_frame_rate = if sub_frame_rate > 0.0 {
            sub_frame_rate
        } else {
            DEFAULT_TTML_SUBFRAME_RATE
        };
        self.params.tick_rate = if tick_rate > 0.0 {
            tick_rate
        } else {
            DEFAULT_TTML_TICK_RATE
        };
    }
}

fn child_context_with(
    attrs: &[(String, String)],
    parent: &TtmlTimingContext,
    params: &TtmlTimingParams,
) -> TtmlTimingContext {
    let mut ctx = parent.clone();

    if let Some(lang) = attr_value(attrs, "lang") {
        ctx.lang = normalize_lyric_lang(lang);
    }
    if let Some(agent_id) = attr_value(attrs, "agent") {
        ctx.agent_id = agent_id.trim().to_string();
    }
    if let Some(role) = attr_value(attrs, "role") {
        let role = role.trim();
        if !role.is_empty() {
            ctx.role = append_ttml_roles(&ctx.role, role);
        }
    }

    let begin_expr = attr_value(attrs, "begin");
    let end_expr = attr_value(attrs, "end");
    let dur_expr = attr_value(attrs, "dur");

    if let Some(begin_expr) = begin_expr {
        let Some((begin, kind)) = parse_ttml_time_expression(begin_expr, params) else {
            ctx.invalid = true;
            return ctx;
        };

        let base = if parent.has_begin { parent.begin } else { 0 };
        ctx.begin = resolve_ttml_time(begin, kind, base, parent);
        ctx.has_begin = true;
    } else {
        ctx.begin = parent.begin;
        ctx.has_begin = parent.has_begin;
    }

    let mut calculated_end: i64 = 0;
    let mut calculated_has_end = false;

    if let Some(end_expr) = end_expr {
        let Some((end, kind)) = parse_ttml_time_expression(end_expr, params) else {
            ctx.invalid = true;
            return ctx;
        };

        let base = if ctx.has_begin {
            ctx.begin
        } else {
            parent.begin
        };
        calculated_end = resolve_ttml_time(end, kind, base, parent);
        calculated_has_end = true;
    }

    if let Some(dur_expr) = dur_expr {
        let Some(dur) = parse_ttml_duration_expression(dur_expr, params) else {
            ctx.invalid = true;
            return ctx;
        };
        if ctx.has_begin {
            let dur_end = ctx.begin + dur;
            if !calculated_has_end || dur_end < calculated_end {
                calculated_end = dur_end;
                calculated_has_end = true;
            }
        }
    }

    if !calculated_has_end && parent.has_end {
        calculated_end = parent.end;
        calculated_has_end = true;
    }

    ctx.end = calculated_end;
    ctx.has_end = calculated_has_end;
    ctx
}

/// `backgroundAgentID`.
fn background_agent_id(agent_id: &str) -> String {
    format!("{TTML_BACKGROUND_AGENT_PREFIX}{agent_id}")
}

/// `isBackgroundAgentID`.
fn is_background_agent_id(agent_id: &str) -> bool {
    agent_id.starts_with(TTML_BACKGROUND_AGENT_PREFIX)
}

/// `contextHasRole`.
fn context_has_role(roles: &str, role: &str) -> bool {
    let lower_role = role.to_lowercase();
    roles
        .to_lowercase()
        .split_whitespace()
        .any(|c| c == lower_role)
}

/// `appendTTMLRoles`.
fn append_ttml_roles(existing: &str, roles: &str) -> String {
    let mut existing = existing.to_string();
    for role in roles.split_whitespace() {
        if context_has_role(&existing, role) {
            continue;
        }
        if existing.is_empty() {
            existing = role.to_string();
        } else {
            existing.push(' ');
            existing.push_str(role);
        }
    }
    existing
}

/// `parseTTMLDurationExpression`.
fn parse_ttml_duration_expression(expr: &str, params: &TtmlTimingParams) -> Option<i64> {
    parse_ttml_time_expression(expr, params).map(|(v, _)| v)
}

/// `resolveTTMLTime`.
fn resolve_ttml_time(value: i64, kind: TtmlTimeKind, base: i64, parent: &TtmlTimingContext) -> i64 {
    match kind {
        TtmlTimeKind::Absolute => value,
        TtmlTimeKind::Offset => base + value,
        TtmlTimeKind::Ambiguous => {
            let absolute = value;
            let offset = base + value;

            // No parent timing context → no reference frame for offsets.
            // Prefer absolute when offset differs (i.e., base > 0).
            if !parent.has_begin && !parent.has_end && base != 0 {
                return absolute;
            }

            if parent.has_begin && parent.has_end {
                let absolute_in_parent = absolute >= parent.begin && absolute <= parent.end;
                let offset_in_parent = offset >= parent.begin && offset <= parent.end;
                if absolute_in_parent && !offset_in_parent {
                    return absolute;
                }
                if offset_in_parent && !absolute_in_parent {
                    return offset;
                }
            }

            if parent.has_begin {
                if absolute < parent.begin && offset >= parent.begin {
                    return offset;
                }
                if absolute >= parent.begin && offset > absolute {
                    return absolute;
                }
            }
            offset
        }
    }
}

/// `parseTTMLTimeExpression`.
fn parse_ttml_time_expression(
    expr: &str,
    params: &TtmlTimingParams,
) -> Option<(i64, TtmlTimeKind)> {
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }

    let lower = expr.to_lowercase();
    if lower.contains("wallclock(") || lower.contains(".begin") || lower.contains(".end") {
        // Go logs a warning for unsupported expressions; the observable
        // behavior is rejection (None → invalid).
        return None;
    }

    // Best-effort support for non-standard TTML seen in the wild where a
    // bare decimal value is used (implicitly seconds), e.g. "0.170".
    if let Ok(value) = lower.parse::<f64>() {
        if value >= 0.0 {
            return Some(((value * 1000.0).round() as i64, TtmlTimeKind::Ambiguous));
        }
    }

    if let Some(m) = offset_time_regex().captures(&lower) {
        let value: f64 = m[1].parse().ok()?;
        let unit = &m[2];
        let seconds = match unit {
            "h" => value * 60.0 * 60.0,
            "m" => value * 60.0,
            "s" => value,
            "ms" => value / 1000.0,
            "f" => value / params.frame_rate,
            "t" => value / params.tick_rate,
            _ => return None,
        };

        return Some(((seconds * 1000.0).round() as i64, TtmlTimeKind::Offset));
    }

    let colon_count = expr.matches(':').count();
    match colon_count {
        1 | 2 => {
            let clock_ms = parse_ttml_clock_time(expr)?;
            Some((clock_ms, TtmlTimeKind::Absolute))
        }
        3 => {
            let frames_ms = parse_ttml_frame_time(expr, params)?;
            Some((frames_ms, TtmlTimeKind::Absolute))
        }
        _ => None,
    }
}

/// `parseTTMLClockTime`.
fn parse_ttml_clock_time(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 2 && parts.len() != 3 {
        return None;
    }

    let mut hours: i64 = 0;
    let mut minutes_idx = 0;
    if parts.len() == 3 {
        hours = parts[0].parse().ok()?;
        minutes_idx = 1;
    }

    let minutes: i64 = parts[minutes_idx].parse().ok()?;
    let seconds: f64 = parts[minutes_idx + 1].parse().ok()?;

    let total_seconds = (hours * 60 * 60 + minutes * 60) as f64 + seconds;
    Some((total_seconds * 1000.0).round() as i64)
}

/// `parseTTMLFrameTime`.
fn parse_ttml_frame_time(value: &str, params: &TtmlTimingParams) -> Option<i64> {
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 4 {
        return None;
    }

    let hours: i64 = parts[0].parse().ok()?;
    let minutes: i64 = parts[1].parse().ok()?;
    let seconds: i64 = parts[2].parse().ok()?;

    let frame_parts: Vec<&str> = parts[3].splitn(2, '.').collect();
    let frames: f64 = frame_parts[0].parse().ok()?;

    let mut sub_frames = 0.0f64;
    if frame_parts.len() == 2 {
        sub_frames = frame_parts[1].parse().ok()?;
    }

    let mut total_seconds = (hours * 60 * 60 + minutes * 60 + seconds) as f64;
    total_seconds += frames / params.frame_rate;
    total_seconds += sub_frames / (params.sub_frame_rate * params.frame_rate);

    Some((total_seconds * 1000.0).round() as i64)
}

/// `sanitizeTTMLText`.
fn sanitize_ttml_text(raw: &str) -> String {
    let raw = sanitize_text(raw);
    let raw = raw.replace("\r\n", "\n").replace('\r', "\n");

    let lines: Vec<String> = raw.split('\n').map(|l| l.trim().to_string()).collect();
    lines.join("\n").trim().to_string()
}

/// `linesAreSynced`.
fn lines_are_synced(lines: &[Line]) -> bool {
    for line in lines {
        if line.start.is_some() {
            return true;
        }
        for cue in &line.cue {
            if cue.start.is_some() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: i64) -> Option<i64> {
        Some(v)
    }

    fn cue(
        start: Option<i64>,
        end: Option<i64>,
        value: &str,
        byte_start: usize,
        byte_end: usize,
        agent_id: &str,
    ) -> Cue {
        Cue {
            start,
            end,
            value: value.into(),
            byte_start,
            byte_end,
            agent_id: agent_id.into(),
        }
    }

    // Port of model/lyrics_ttml_test.go
    #[test]
    fn parses_multiple_language_divs_with_inherited_offsets_and_frame_tick_timing() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" ttp:frameRate="30" ttp:subFrameRate="2" ttp:tickRate="10">
  <body>
    <div xml:lang="eng" begin="1s">
      <p begin="2s">Line one</p>
      <p begin="00:00:04:15.1"><span>Line two</span><br/>with break</p>
    </div>
    <div xml:lang="por">
      <p begin="45t">Linha</p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 2);

        let eng = &list[0];
        assert_eq!(eng.lang, "eng");
        assert!(eng.synced);
        assert_eq!(eng.line[0].start, p(3000));
        assert_eq!(eng.line[0].value, "Line one");
        assert_eq!(eng.line[1].start, p(4517));
        assert_eq!(eng.line[1].value, "Line two\nwith break");

        let por = &list[1];
        assert_eq!(por.lang, "por");
        assert_eq!(por.line[0].start, p(4500));
        assert_eq!(por.line[0].value, "Linha");
    }

    #[test]
    fn skips_wallclock_cues_and_keeps_valid_ones() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body xml:lang="eng">
    <div>
      <p begin="wallclock(2026-01-01T00:00:00Z)">Skip me</p>
      <p begin="1s">Keep me</p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].start, p(1000));
        assert_eq!(list[0].line[0].value, "Keep me");
    }

    #[test]
    fn accumulates_nested_timing_from_body_div_and_p() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body xml:lang="eng" begin="10s">
    <div begin="5s" dur="8s">
      <p begin="1s" dur="2s">First line</p>
      <p begin="3s" end="5s">Second line</p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].lang, "eng");
        assert_eq!(list[0].line.len(), 2);
        assert_eq!(list[0].line[0].start, p(16000));
        assert_eq!(list[0].line[0].value, "First line");
        assert_eq!(list[0].line[1].start, p(18000));
        assert_eq!(list[0].line[1].value, "Second line");
    }

    #[test]
    fn parses_bare_decimal_numbers_as_seconds() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body xml:lang="eng" begin="10">
    <div>
      <p begin="0.170">First line</p>
      <p begin="3.710">Second line</p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line.len(), 2);
        assert_eq!(list[0].line[0].start, p(10170));
        assert_eq!(list[0].line[0].value, "First line");
        assert_eq!(list[0].line[1].start, p(13710));
        assert_eq!(list[0].line[1].value, "Second line");
    }

    #[test]
    fn extracts_timed_tokens_from_spans_including_background_role() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <body xml:lang="eng">
    <div>
      <p begin="00:01.000" end="00:03.000">
        <span begin="00:01.000" end="00:01.400">He</span><span begin="00:01.400" end="00:01.800">llo</span>
        <span ttm:role="x-bg"><span begin="00:02.000" end="00:02.500">echo</span></span>
      </p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].agents,
            vec![
                Agent {
                    id: "main".into(),
                    role: "main".into(),
                    ..Default::default()
                },
                Agent {
                    id: "__nd_bg__|main".into(),
                    role: "bg".into(),
                    ..Default::default()
                },
            ]
        );
        assert_eq!(list[0].line.len(), 1);

        let line = &list[0].line[0];
        assert_eq!(line.start, p(1000));
        assert_eq!(line.value, "Hello echo");
        assert_eq!(line.end, p(3000));
        assert_eq!(line.cue.len(), 3);

        assert_eq!(line.cue[0], cue(p(1000), p(1400), "He", 0, 1, "main"));
        assert_eq!(line.cue[1], cue(p(1400), p(1800), "llo", 2, 4, "main"));
        assert_eq!(
            line.cue[2],
            cue(p(2000), p(2500), "echo", 6, 9, "__nd_bg__|main")
        );
    }

    #[test]
    fn appends_role_tokens_exactly_instead_of_substring_matches() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <body xml:lang="eng">
    <div>
      <p begin="00:01.000" end="00:03.000" ttm:role="not-x-bg"><span begin="00:01.000" end="00:01.400">Lead</span><span ttm:role="x-bg"><span begin="00:02.000" end="00:02.500">Echo</span></span></p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].agents,
            vec![
                Agent {
                    id: "main".into(),
                    role: "main".into(),
                    ..Default::default()
                },
                Agent {
                    id: "__nd_bg__|main".into(),
                    role: "bg".into(),
                    ..Default::default()
                },
            ]
        );
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].cue.len(), 2);
        assert_eq!(list[0].line[0].cue[0].agent_id, "main");
        assert_eq!(list[0].line[0].cue[1].agent_id, "__nd_bg__|main");
    }

    #[test]
    fn parses_named_ttml_agents_into_main_voice_and_group_roles() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <head>
    <metadata>
      <ttm:agent xml:id="v1" type="person"><ttm:name>Chris Martin</ttm:name></ttm:agent>
      <ttm:agent xml:id="v2" type="person"><ttm:name>Jin</ttm:name></ttm:agent>
      <ttm:agent xml:id="v1000" type="group"><ttm:name>All</ttm:name></ttm:agent>
    </metadata>
  </head>
  <body xml:lang="eng">
    <div>
      <p begin="1s" end="2s" ttm:agent="v1"><span begin="1s" end="1.5s">You</span></p>
      <p begin="2s" end="3s" ttm:agent="v2"><span begin="2s" end="2.5s">and</span></p>
      <p begin="3s" end="4s" ttm:agent="v1000"><span begin="3s" end="3.5s">All</span></p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].agents,
            vec![
                Agent {
                    id: "v1".into(),
                    role: "main".into(),
                    name: "Chris Martin".into()
                },
                Agent {
                    id: "v2".into(),
                    role: "voice".into(),
                    name: "Jin".into()
                },
                Agent {
                    id: "v1000".into(),
                    role: "group".into(),
                    name: "All".into()
                },
            ]
        );
        assert_eq!(list[0].line[0].cue[0].agent_id, "v1");
        assert_eq!(list[0].line[1].cue[0].agent_id, "v2");
        assert_eq!(list[0].line[2].cue[0].agent_id, "v1000");
    }

    #[test]
    fn avoids_collisions_between_derived_background_and_explicit_agent_ids() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <head>
    <metadata>
      <ttm:agent xml:id="lead" type="person"><ttm:name>Lead</ttm:name></ttm:agent>
      <ttm:agent xml:id="lead__bg" type="person"><ttm:name>Existing Background Id</ttm:name></ttm:agent>
    </metadata>
  </head>
  <body xml:lang="eng">
    <div>
      <p begin="1s" end="2s" ttm:agent="lead">
        <span begin="1s" end="1.4s">Lead</span>
        <span ttm:role="x-bg"><span begin="1.5s" end="1.8s">Echo</span></span>
      </p>
      <p begin="2s" end="3s" ttm:agent="lead__bg">
        <span begin="2s" end="2.5s">Named</span>
      </p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].agents,
            vec![
                Agent {
                    id: "lead".into(),
                    role: "main".into(),
                    name: "Lead".into()
                },
                Agent {
                    id: "__nd_bg__|lead".into(),
                    role: "bg".into(),
                    name: "Lead".into()
                },
                Agent {
                    id: "lead__bg".into(),
                    role: "voice".into(),
                    name: "Existing Background Id".into()
                },
            ]
        );
        assert_eq!(list[0].line.len(), 2);
        assert_eq!(list[0].line[0].cue.len(), 2);
        assert_eq!(list[0].line[0].cue[0].agent_id, "lead");
        assert_eq!(list[0].line[0].cue[1].agent_id, "__nd_bg__|lead");
        assert_eq!(list[0].line[1].cue.len(), 1);
        assert_eq!(list[0].line[1].cue[0].agent_id, "lead__bg");
    }

    #[test]
    fn fills_missing_cue_agent_ids_with_the_resolved_main_agent() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <head>
    <metadata>
      <ttm:agent xml:id="guest" type="person"><ttm:name>Guest Vocal</ttm:name></ttm:agent>
    </metadata>
  </head>
  <body xml:lang="eng">
    <div>
      <p begin="1s" end="3s">
        <span begin="1s" end="1.4s">Lead</span>
        <span begin="2s" end="2.4s" ttm:agent="guest">Guest</span>
      </p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].agents,
            vec![Agent {
                id: "guest".into(),
                role: "main".into(),
                name: "Guest Vocal".into()
            }]
        );
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].cue.len(), 2);
        assert_eq!(list[0].line[0].cue[0].agent_id, "guest");
        assert_eq!(list[0].line[0].cue[1].agent_id, "guest");
    }

    #[test]
    fn collapses_pretty_print_indentation_into_single_spaces() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <body xml:lang="eng">
    <div>
      <p begin="1:22.889" end="1:26.859" ttm:agent="v2">
        <span begin="1:22.889" end="1:23.127">It</span>
        <span begin="1:23.374" end="1:23.938">in,</span>
        <span ttm:role="x-bg">
          <span begin="1:23.881" end="1:24.243">(When you</span>
          <span begin="1:26.232" end="1:26.859">slide)</span>
        </span>
      </p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line.len(), 1);

        let line = &list[0].line[0];
        assert_eq!(line.value, "It in, (When you slide)");
        assert!(!line.value.contains('\n'));
        assert_eq!(line.cue.len(), 4);
        assert_eq!(line.cue[0], cue(p(82889), p(83127), "It", 0, 1, "v2"));
        assert_eq!(line.cue[1], cue(p(83374), p(83938), "in,", 3, 5, "v2"));
        assert_eq!(
            line.cue[2],
            cue(p(83881), p(84243), "(When you", 7, 15, "__nd_bg__|v2")
        );
        assert_eq!(
            line.cue[3],
            cue(p(86232), p(86859), "slide)", 17, 22, "__nd_bg__|v2")
        );
    }

    #[test]
    fn preserves_explicit_br_as_a_line_break() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <body xml:lang="eng">
    <div>
      <p begin="00:01.000" end="00:03.000">
        <span begin="00:01.000" end="00:01.400">first</span>
        <br/>
        <span begin="00:02.000" end="00:02.500">second</span>
      </p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].value, "first\nsecond");
    }

    #[test]
    fn only_collapses_xml_whitespace_leaving_other_unicode_spaces_intact() {
        // Whitespace collapsing only touches the XML S characters
        // (space/tab/CR/LF). Other Unicode spaces like U+3000 are left as-is.
        let content = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tt xmlns=\"http://www.w3.org/ns/ttml\">\n  <body xml:lang=\"jpn\">\n    <div>\n      <p begin=\"00:01.000\" end=\"00:03.000\">\n        <span begin=\"00:01.000\" end=\"00:01.400\">\u{3042}\u{3000}\u{3044}</span>\n        <span begin=\"00:02.000\" end=\"00:02.500\">\u{3046}</span>\n      </p>\n    </div>\n  </body>\n</tt>".as_bytes();

        let list = parse_ttml("xxx", content).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].value, "\u{3042}\u{3000}\u{3044} \u{3046}");
        assert_eq!(list[0].line[0].cue[0].value, "\u{3042}\u{3000}\u{3044}");
    }

    #[test]
    fn does_not_corrupt_main_cue_end_when_background_cue_is_earlier() {
        // Background spans (x-bg) appear after the main spans in document order
        // but their timings interleave with the main timeline. End-time
        // normalization must be per agent so the last main cue keeps its real
        // end instead of collapsing to its own start.
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
  <body xml:lang="eng">
    <div>
      <p begin="1:22.889" end="1:26.859" ttm:agent="v2"><span begin="1:25.593" end="1:25.934">real</span> <span begin="1:25.934" end="1:26.751">slow</span> <span ttm:role="x-bg"><span begin="1:23.881" end="1:24.243">(When you</span> <span begin="1:26.232" end="1:26.859">slide)</span></span></p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line.len(), 1);

        let line = &list[0].line[0];
        assert_eq!(line.cue.len(), 4);

        let mut main_cues: Vec<&Cue> = Vec::new();
        let mut bg_cues: Vec<&Cue> = Vec::new();
        for c in &line.cue {
            if c.agent_id == "v2" {
                main_cues.push(c);
            } else {
                bg_cues.push(c);
            }
        }

        assert_eq!(main_cues.len(), 2);
        assert_eq!(main_cues[0].end, p(85934)); // "real"
                                                // "slow" must keep its real end (86751), not collapse to its start.
        assert_eq!(main_cues[1].start, p(85934));
        assert_eq!(main_cues[1].end, p(86751));

        assert_eq!(bg_cues.len(), 2);
        assert_eq!(bg_cues[0].end, p(84243)); // "(When you"
        assert_eq!(bg_cues[1].end, p(86859)); // "slide)"
    }

    #[test]
    fn prefers_absolute_timing_when_values_fall_inside_parent_window() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body xml:lang="eng">
    <div begin="37.870" end="45.570">
      <p begin="43.444" end="45.570">
        <span begin="43.444" end="43.716">go</span>
        <span begin="43.716" end="43.887">go</span>
      </p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].line.len(), 1);

        let line = &list[0].line[0];
        assert_eq!(line.start, p(43444));
        assert_eq!(line.value, "go go");
        assert_eq!(line.end, p(45570));
        assert_eq!(line.cue.len(), 2);
        assert_eq!(line.cue[0], cue(p(43444), p(43716), "go", 0, 1, ""));
        assert_eq!(line.cue[1], cue(p(43716), p(43887), "go", 3, 4, ""));
    }

    #[test]
    fn returns_unsynced_lyrics_when_no_timing_is_present() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body>
    <div>
      <p>No timing here</p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].lang, "xxx");
        assert!(!list[0].synced);
        assert_eq!(list[0].line.len(), 1);
        assert_eq!(list[0].line[0].start, None);
        assert_eq!(list[0].line[0].value, "No timing here");
    }

    #[test]
    fn produces_main_translation_and_pronunciation_tracks() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal">
  <head>
    <metadata>
      <iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal">
        <translations>
          <translation xml:lang="es">
            <text for="L1">Hola</text>
            <text for="MISSING">Skip me</text>
          </translation>
        </translations>
        <transliterations>
          <transliteration xml:lang="ja-Latn">
            <text for="L2"><span begin="00:02.000" end="00:02.300" xmlns="http://www.w3.org/ns/ttml">ko</span><span begin="00:02.300" end="00:02.600" xmlns="http://www.w3.org/ns/ttml">nni</span></text>
          </transliteration>
        </transliterations>
      </iTunesMetadata>
    </metadata>
  </head>
  <body xml:lang="ja">
    <div>
      <p begin="00:01.000" end="00:01.500" itunes:key="L1">こんにちは</p>
      <p begin="00:02.000" end="00:02.700" itunes:key="L2">こんばんは</p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();
        assert_eq!(list.len(), 3);

        let main = &list[0];
        assert_eq!(main.kind, "main");
        assert_eq!(main.lang, "ja");
        assert_eq!(main.line.len(), 2);

        let translation = &list[1];
        assert_eq!(translation.kind, "translation");
        assert_eq!(translation.lang, "es");
        assert_eq!(translation.line.len(), 1);
        assert_eq!(translation.line[0].start, p(1000));
        assert_eq!(translation.line[0].value, "Hola");
        assert_eq!(translation.line[0].end, p(1500));

        let pronunciation = &list[2];
        assert_eq!(pronunciation.kind, "pronunciation");
        assert_eq!(pronunciation.lang, "ja-latn");
        assert_eq!(pronunciation.line.len(), 1);
        assert_eq!(pronunciation.line[0].start, p(2000));
        assert_eq!(pronunciation.line[0].value, "konni");
        assert_eq!(pronunciation.line[0].end, p(2600));
        assert_eq!(pronunciation.line[0].cue.len(), 2);
        assert_eq!(
            pronunciation.line[0].cue[0],
            cue(p(2000), p(2300), "ko", 0, 1, "")
        );
        assert_eq!(
            pronunciation.line[0].cue[1],
            cue(p(2300), p(2600), "nni", 2, 4, "")
        );
    }

    #[test]
    fn parses_bare_decimal_times_in_transliteration_spans() {
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal">
  <head>
    <metadata>
      <iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal">
        <transliterations>
          <transliteration xml:lang="ja-Latn">
            <text for="L1"><span begin="2.747" end="3.018" xmlns="http://www.w3.org/ns/ttml">I</span> <span begin="3.018" end="3.179" xmlns="http://www.w3.org/ns/ttml">woke</span> <span begin="3.179" end="3.582" xmlns="http://www.w3.org/ns/ttml">up</span></text>
          </transliteration>
        </transliterations>
      </iTunesMetadata>
    </metadata>
  </head>
  <body xml:lang="ja">
    <div>
      <p begin="00:02.747" end="00:04.000" itunes:key="L1">起きた</p>
    </div>
  </body>
</tt>"#;

        let list = parse_ttml("xxx", content.as_bytes()).unwrap().unwrap();

        let pronunciation = list
            .iter()
            .find(|l| l.kind == "pronunciation")
            .expect("pronunciation track");
        assert_eq!(pronunciation.line.len(), 1);

        let line = &pronunciation.line[0];
        assert_eq!(line.start, p(2747));
        assert_eq!(line.value, "I woke up");
        assert_eq!(line.cue.len(), 3);
        assert_eq!(line.cue[0], cue(p(2747), p(3018), "I", 0, 0, ""));
        assert_eq!(line.cue[1], cue(p(3018), p(3179), "woke", 2, 5, ""));
        assert_eq!(line.cue[2], cue(p(3179), p(3582), "up", 7, 8, ""));
    }
}
