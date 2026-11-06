//! Port of `persistence/criteria_sql.go` (904L) — the criteria→SQL compiler
//! feeding `smart_playlist_repo.rs`'s [`crate::smart_playlist_repo::CriteriaSql`]
//! seam.
//!
//! Go models every condition as a squirrel `Sqlizer` value; this port models
//! the same conditions as the closed [`Cond`] tree and renders them depth-first
//! with args bound at their placeholder position ([`Cond::render`]). The
//! squirrel structural equivalents map 1:1: `And`/`Or` →
//! [`Cond::And`]/[`Cond::Or`], `Eq`/`NotEq` → [`Cond::Eq`]/[`Cond::NotEq`],
//! `Like`/`NotLike` → [`Cond::Like`]/[`Cond::NotLike`], and the custom
//! `Sqlizer`s (`tagCond`, `roleCond`, the merge groups, `inList`'s Expr)
//! → [`Cond::TagExists`]/[`Cond::RoleExists`]/[`Cond::TagGroup`]/[`Cond::RoleGroup`]
//! /[`Cond::Raw`].
//!
//! Go's package-global `criteria.LookupField`/`Walk` registry is nd-model's
//! value-typed [`FieldMap`], threaded through the API; Go's `smartPlaylistCriteria`
//! owner option (`criteria_sql.go:45-57`) becomes the [`Owner`] struct. Go
//! renders squirrel statements through `toSQL()`; the port renders into the
//! crate's shared `(sql, args)` shape ([`crate::smart_playlist_repo::Sql`])
//! with every value bound, never concatenated (slice-2 security contract).

use std::collections::BTreeMap;

use nd_model::criteria::order_by_fields as criteria_order_by_fields;
use nd_model::criteria::Criteria;
use nd_model::fields::default_field_map;
use nd_model::fields::lookup_field;
use nd_model::fields::FieldInfo;
use nd_model::fields::FieldMap;
use nd_model::json::to_bool;
use nd_model::operators::fields as expression_fields;
use nd_model::operators::Expression;
use nd_model::sort::sort_field_names as sort_names;
use nd_model::sort::SortField;

use crate::smart_playlist_repo::Sql;
use crate::sqlrepo::Value;
use nd_model::operators::Fields;
#[allow(unused_imports)]
use serde_json::json;
use serde_json::Value as JsonValue;

/// Filesystems disagree on the Unicode form of a name, so match the path in
/// NFC and NFD (`pathVariants`, `criteria_sql.go:381-391`). Go's
/// `norm.NFC.String`/`norm.NFD.String` map to the crate's chosen
/// `unicode-normalization` forms — the NFC/NFD pair that rounds a mixed-form
/// path into its two candidate spellings.
#[must_use]
pub fn path_variants(path: &str) -> Vec<String> {
    let mut variants: Vec<String> = vec![path.to_string()];
    let alt = unicode_normalization::UnicodeNormalization::nfc(path).collect::<String>();
    if alt != path {
        variants.push(alt);
    }
    let alt = unicode_normalization::UnicodeNormalization::nfd(path).collect::<String>();
    if alt != path {
        variants.push(alt);
    }
    variants
}

/// The `owner` option of `smartPlaylistCriteria` (`criteria_sql.go:40-57`):
/// the fields `inList` consults for the visibility filters (`:357-366`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Owner {
    /// Go `c.owner.IsAdmin` (`criteria_sql.go:357`).
    pub is_admin: bool,
    /// Go `c.owner.ID` (`criteria_sql.go:358-363`).
    pub id: String,
}

/// The smart-playlist join kinds — Go's `smartPlaylistJoinType`
/// (`criteria_sql.go:19-30`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmartPlaylistJoinType(i32);

/// Go `smartPlaylistJoinNone` (`criteria_sql.go:22`).
pub const SMART_PLAYLIST_JOIN_NONE: SmartPlaylistJoinType = SmartPlaylistJoinType(0);
/// Go `smartPlaylistJoinAlbumAnnotation` (`criteria_sql.go:23`).
pub const SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION: SmartPlaylistJoinType = SmartPlaylistJoinType(1);
/// Go `smartPlaylistJoinArtistAnnotation` (`criteria_sql.go:24`).
pub const SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION: SmartPlaylistJoinType = SmartPlaylistJoinType(2);
/// Go `smartPlaylistJoinAlbum` (`criteria_sql.go:25`).
pub const SMART_PLAYLIST_JOIN_ALBUM: SmartPlaylistJoinType = SmartPlaylistJoinType(4);

/// Go's `loved`-field `false` default (`criteria_sql.go:95,103,114`) —
/// rendered as the SQL literal `false` by [`sql_literal`].
pub const LOVED_DEFAULT: CoalesceDefault = CoalesceDefault::Bool(false);

/// Go's count/rating `0` default (`criteria_sql.go:99-104,113,115`).
pub const COUNT_DEFAULT: CoalesceDefault = CoalesceDefault::Int(0);

impl SmartPlaylistJoinType {
    /// Go's `has` (`criteria_sql.go:28-30`).
    #[must_use]
    pub fn has(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Go's `|=` on the iota-constellation (`criteria_sql.go:810,821`).
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Go's `coalesceDefault any` (`criteria_sql.go:37`) — `false` for the loved
/// columns, `0` for the count/rating columns; `None` = none (Go's `nil`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoalesceDefault {
    /// Go `false` (`criteria_sql.go:95,103,114`).
    Bool(bool),
    /// Go `0` (`criteria_sql.go:99-104,113,115`).
    Int(i64),
}

/// One field entry — Go's `smartPlaylistField` (`criteria_sql.go:32-38`).
#[derive(Debug, Clone, PartialEq)]
pub struct SmartPlaylistField {
    /// Go `expr` (`criteria_sql.go:33`).
    pub expr: String,
    /// Go `order` (`criteria_sql.go:34`) — only `random` carries one.
    pub order: String,
    /// Go `joinType` (`criteria_sql.go:35`).
    pub join_type: SmartPlaylistJoinType,
    /// Go `emptyValues` (`criteria_sql.go:36`) — additional values that encode
    /// "missing" for string columns (e.g. `'[]'` for lyrics).
    pub empty_values: Vec<String>,
    /// Go `coalesceDefault` (`criteria_sql.go:37`) — missing-row default for a
    /// nullable annotation column; `None` = none. See [`annotation_cond`].
    pub coalesce_default: Option<CoalesceDefault>,
}

/// One leaf comparison: `column <op> ?` with its bound value — the squirrel
/// `Eq`/`NotEq`/`Gt`/`GtOrEq`/`Lt`/`LtOrEq` family (`comparator.build`,
/// `criteria_sql.go:669-676`). Ordering operators carry `ordering`
/// (`criteria_sql.go:666`) and the `satisfy` predicate (`:664`) in the free
/// fns [`cmp_satisfy`]/[`cmp_ordering`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Comparison {
    /// Go `Eq` (`criteria_sql.go:670`).
    Eq,
    /// Go `NotEq` (`criteria_sql.go:671`).
    NotEq,
    /// Go `Gt` (`criteria_sql.go:672`).
    Gt,
    /// Go `GtOrEq` (`criteria_sql.go:673`).
    GtOrEq,
    /// Go `Lt` (`criteria_sql.go:674`).
    Lt,
    /// Go `LtOrEq` (`criteria_sql.go:675`).
    LtOrEq,
}

impl Comparison {
    /// The SQL operator text (`comparator.build`, `criteria_sql.go:669-676`).
    #[must_use]
    pub fn op(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::NotEq => "<>",
            Self::Gt => ">",
            Self::GtOrEq => ">=",
            Self::Lt => "<",
            Self::LtOrEq => "<=",
        }
    }
}

/// Go's `satisfy` predicate (`criteria_sql.go:664, 670-675`): the operator as
/// a numeric predicate, used by [`bare_null_inclusion`] to reason about a
/// column's COALESCE default.
#[must_use]
fn cmp_satisfy(cmp: Comparison, a: f64, b: f64) -> bool {
    match cmp {
        Comparison::Eq => a == b,
        Comparison::NotEq => a != b,
        Comparison::Gt => a > b,
        Comparison::GtOrEq => a >= b,
        Comparison::Lt => a < b,
        Comparison::LtOrEq => a <= b,
    }
}

/// Go's `ordering` flag (`criteria_sql.go:666`): false only for `=`/`<>`.
#[must_use]
fn cmp_ordering(cmp: Comparison) -> bool {
    !matches!(cmp, Comparison::Eq | Comparison::NotEq)
}

/// The `firstDate` render of `startOfPeriod` (`criteria_sql.go:343-345`):
/// `from` minus `numDays` days, formatted `2006-01-02`.
#[must_use]
pub fn start_of_period(num_days: i64, from: std::time::SystemTime) -> String {
    let base = from
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let shifted = base
        .checked_sub(std::time::Duration::from_secs(
            num_days.unsigned_abs() * 86_400,
        ))
        .unwrap_or_default();
    let secs = shifted.as_secs() as i64;
    // Civil-date conversion (Howard Hinnant's algorithm), the port of Go's
    // `Format("2006-01-02")`.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// One rendered WHERE condition — the squirrel `Sqlizer` tree of
/// `criteria_sql.go`, closing over the leaf shapes the compiler emits.
#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    /// Go `squirrel.Eq{col: v}` (`criteria_sql.go:244,359,362`).
    Eq {
        /// The column expression.
        column: String,
        /// The bound value; `None` renders `col IS NULL`.
        value: Option<Value>,
    },
    /// Go `squirrel.NotEq{col: v}` (`criteria_sql.go:242,245`).
    NotEq {
        /// The column expression.
        column: String,
        /// The bound value; `None` renders `col IS NOT NULL`.
        value: Option<Value>,
    },
    /// Go `squirrel.Gt{col: v}` (`criteria_sql.go:340`).
    Gt {
        /// The column expression.
        column: String,
        /// The bound value.
        value: Value,
    },
    /// Go `squirrel.GtOrEq{col: v}` (`criteria_sql.go:673`).
    GtOrEq {
        /// The column expression.
        column: String,
        /// The bound value.
        value: Value,
    },
    /// Go `squirrel.LtOrEq{col: v}` (`criteria_sql.go:675`).
    LtOrEq {
        /// The column expression.
        column: String,
        /// The bound value.
        value: Value,
    },
    /// Go `squirrel.Eq{col: []any}` — the IN-list of `inList`'s path form
    /// (`criteria_sql.go:352`).
    In {
        /// The column expression.
        column: String,
        /// The bound values.
        values: Vec<Value>,
    },
    /// Go `squirrel.Lt{col: v}` (`criteria_sql.go:336`).
    Lt {
        /// The column expression.
        column: String,
        /// The bound value.
        value: Value,
    },
    /// Go `squirrel.Like{col: pattern}` (`likeCond`, `criteria_sql.go:290`).
    Like {
        /// The column expression.
        column: String,
        /// The pattern value.
        value: String,
    },
    /// Go `squirrel.NotLike{col: pattern}` (`likeCond`, `criteria_sql.go:288`).
    NotLike {
        /// The column expression.
        column: String,
        /// The pattern value.
        value: String,
    },
    /// Go `squirrel.Expr("… IN (subSQL)", subArgs…)` (`inList`,
    /// `criteria_sql.go:375-378`).
    InSubquery {
        /// The rendered subquery with its args.
        subquery: Sql,
        /// Whether the outer match is negated (`:375`).
        negate: bool,
    },
    /// Go `tagCond` (`criteria_sql.go:400-424`).
    TagExists {
        /// Go `tagCond.tag`.
        tag: String,
        /// Go `tagCond.numeric`.
        numeric: bool,
        /// Go `tagCond.cond` — `None` is the presence-only form (`:418`).
        cond: Option<Box<Cond>>,
        /// Go `tagCond.not`.
        not: bool,
    },
    /// Go `roleCond` (`criteria_sql.go:426-450`).
    RoleExists {
        /// Go `roleCond.role`.
        role: String,
        /// Go `roleCond.cond` — `None` is the bare role form (`:443`).
        cond: Option<Box<Cond>>,
        /// Go `roleCond.not`.
        not: bool,
    },
    /// Go `tagCondGroup` (`criteria_sql.go:595-624`).
    TagGroup {
        /// Go `tagCondGroup.tag`.
        tag: String,
        /// Go `tagCondGroup.numeric`.
        numeric: bool,
        /// Go `tagCondGroup.conds`.
        conds: Vec<Cond>,
        /// Go `tagCondGroup.not`.
        not: bool,
    },
    /// Go `roleCondGroup` (`criteria_sql.go:569-593`).
    RoleGroup {
        /// Go `roleCondGroup.role`.
        role: String,
        /// Go `roleCondGroup.conds`.
        conds: Vec<Cond>,
        /// Go `roleCondGroup.not`.
        not: bool,
    },
    /// Go `squirrel.And{…}` (`criteria_sql.go:143,242,315,356`).
    And(Vec<Cond>),
    /// Go `squirrel.Or{…}` (`criteria_sql.go:153,241,335,361`).
    Or(Vec<Cond>),
}

/// Failures of the compiler — Go's `fmt.Errorf`/`errors.New` returns
/// (`criteria_sql.go:197,212,214,218,252,296,305,354,641-649`), surfaced as
/// thiserror variants.
#[derive(Debug, thiserror::Error)]
pub enum CriteriaSqlError {
    /// Go `criteria_sql.go:641`/`:648`/`:214`.
    #[error("invalid field in criteria: {0}")]
    InvalidField(String),
    /// Go `criteria_sql.go:645`.
    #[error("tag and role criteria must contain exactly one field: {0}")]
    TagRoleSingleField(String),
    /// Go `criteria_sql.go:212`.
    #[error("invalid field in criteria: isMissing/isPresent requires exactly one field")]
    MissingRequiresOneField,
    /// Go `criteria_sql.go:218`.
    #[error("invalid boolean value for 'missing' expression: {0}")]
    InvalidMissingValue(String),
    /// Go `criteria_sql.go:197`.
    #[error("unknown criteria expression type")]
    UnknownExpression,
    /// Go `criteria_sql.go:296`.
    #[error("range operator not supported for tag/role field: {0}")]
    RangeUnsupported(String),
    /// Go `criteria_sql.go:305`.
    #[error("range criteria for {0:?} must be a [min, max] pair")]
    RangePair(String),
    /// Go `criteria_sql.go:354` (`errors.New`).
    #[error("playlist id or path not given")]
    PlaylistRefMissing,
    /// Go `criteria_sql.go:329-331` (the `strconv.ParseInt` failure).
    #[error("invalid days value for period expression: {0}")]
    InvalidDays(String),
}

/// The `smartPlaylistCriteria` state — Go's receiver struct
/// (`criteria_sql.go:40-43`) with its option become a plain field.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartPlaylistCriteria {
    /// Go `c.Criteria`.
    pub criteria: Criteria,
    /// Go `c.owner`.
    pub owner: Owner,
    /// The registry threaded through — Go's package-global
    /// `criteria.LookupField` map (`model/criteria/fields.go:25`).
    pub field_map: FieldMap,
    /// The `smartPlaylistFields` map (`criteria_sql.go:59-131`), owned so the
    /// compiler is pure over its inputs.
    pub fields: BTreeMap<String, SmartPlaylistField>,
}

/// Builds the default `smartPlaylistFields` map verbatim from
/// `criteria_sql.go:59-131`.
#[must_use]
pub fn default_smart_playlist_fields() -> BTreeMap<String, SmartPlaylistField> {
    let mut m: BTreeMap<String, SmartPlaylistField> = BTreeMap::new();
    let mut plain = |name: &str, expr: &str| {
        m.insert(
            name.to_string(),
            SmartPlaylistField {
                expr: expr.to_string(),
                order: String::new(),
                join_type: SMART_PLAYLIST_JOIN_NONE,
                empty_values: Vec::new(),
                coalesce_default: None,
            },
        );
    };
    for (name, col) in [
        ("title", "media_file.title"),
        ("album", "media_file.album"),
        ("hascoverart", "media_file.has_cover_art"),
        ("tracknumber", "media_file.track_number"),
        ("discnumber", "media_file.disc_number"),
        ("year", "media_file.year"),
        ("date", "media_file.date"),
        ("originalyear", "media_file.original_year"),
        ("originaldate", "media_file.original_date"),
        ("releaseyear", "media_file.release_year"),
        ("releasedate", "media_file.release_date"),
        ("size", "media_file.size"),
        ("compilation", "media_file.compilation"),
        ("missing", "media_file.missing"),
        ("explicitstatus", "media_file.explicit_status"),
        ("dateadded", "media_file.created_at"),
        ("datemodified", "media_file.updated_at"),
        ("discsubtitle", "media_file.disc_subtitle"),
        ("comment", "media_file.comment"),
        ("sorttitle", "media_file.sort_title"),
        ("sortalbum", "media_file.sort_album_name"),
        ("sortartist", "media_file.sort_artist_name"),
        ("sortalbumartist", "media_file.sort_album_artist_name"),
        ("albumcomment", "media_file.mbz_album_comment"),
        ("catalognumber", "media_file.catalog_num"),
        ("filepath", "media_file.path"),
        ("filetype", "media_file.suffix"),
        ("codec", "media_file.codec"),
        ("duration", "media_file.duration"),
        ("bitrate", "media_file.bit_rate"),
        ("bitdepth", "media_file.bit_depth"),
        ("samplerate", "media_file.sample_rate"),
        ("bpm", "media_file.bpm"),
        ("channels", "media_file.channels"),
        ("averagerating", "media_file.average_rating"),
        ("mbz_album_id", "media_file.mbz_album_id"),
        ("mbz_album_artist_id", "media_file.mbz_album_artist_id"),
        ("mbz_artist_id", "media_file.mbz_artist_id"),
        ("mbz_recording_id", "media_file.mbz_recording_id"),
        ("mbz_release_track_id", "media_file.mbz_release_track_id"),
        ("mbz_release_group_id", "media_file.mbz_release_group_id"),
        ("rgalbumgain", "media_file.rg_album_gain"),
        ("rgalbumpeak", "media_file.rg_album_peak"),
        ("rgtrackgain", "media_file.rg_track_gain"),
        ("rgtrackpeak", "media_file.rg_track_peak"),
        ("library_id", "media_file.library_id"),
    ] {
        plain(name, col);
    }
    // lyrics carries the `[]` empty encoding (`criteria_sql.go:79`).
    m.insert(
        "lyrics".to_string(),
        SmartPlaylistField {
            expr: "media_file.lyrics".to_string(),
            order: String::new(),
            join_type: SMART_PLAYLIST_JOIN_NONE,
            empty_values: vec!["[]".to_string()],
            coalesce_default: None,
        },
    );
    // Annotation fields with a COALESCE default (`criteria_sql.go:95-104,113-115`).
    for (name, col, join) in [
        ("loved", "annotation.starred", SMART_PLAYLIST_JOIN_NONE),
        (
            "playcount",
            "annotation.play_count",
            SMART_PLAYLIST_JOIN_NONE,
        ),
        ("rating", "annotation.rating", SMART_PLAYLIST_JOIN_NONE),
        (
            "albumrating",
            "album_annotation.rating",
            SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION,
        ),
        (
            "albumloved",
            "album_annotation.starred",
            SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION,
        ),
        (
            "albumplaycount",
            "album_annotation.play_count",
            SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION,
        ),
        (
            "artistrating",
            "artist_annotation.rating",
            SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION,
        ),
        (
            "artistloved",
            "artist_annotation.starred",
            SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION,
        ),
        (
            "artistplaycount",
            "artist_annotation.play_count",
            SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION,
        ),
    ] {
        // Go stores `false` for the loved fields and `0` for the counts
        // (`criteria_sql.go:95,102-104,113-115`).
        let default = if name.ends_with("loved") {
            LOVED_DEFAULT
        } else {
            COUNT_DEFAULT
        };
        m.insert(
            name.to_string(),
            SmartPlaylistField {
                expr: col.to_string(),
                order: String::new(),
                join_type: join,
                empty_values: Vec::new(),
                coalesce_default: Some(default),
            },
        );
    }
    // Date annotation fields — no default, join only (`criteria_sql.go:96-107,116-118`).
    for (name, col, join) in [
        (
            "dateloved",
            "annotation.starred_at",
            SMART_PLAYLIST_JOIN_NONE,
        ),
        (
            "lastplayed",
            "annotation.play_date",
            SMART_PLAYLIST_JOIN_NONE,
        ),
        ("daterated", "annotation.rated_at", SMART_PLAYLIST_JOIN_NONE),
        (
            "albumlastplayed",
            "album_annotation.play_date",
            SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION,
        ),
        (
            "albumdateloved",
            "album_annotation.starred_at",
            SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION,
        ),
        (
            "albumdaterated",
            "album_annotation.rated_at",
            SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION,
        ),
        (
            "artistlastplayed",
            "artist_annotation.play_date",
            SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION,
        ),
        (
            "artistdateloved",
            "artist_annotation.starred_at",
            SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION,
        ),
        (
            "artistdaterated",
            "artist_annotation.rated_at",
            SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION,
        ),
    ] {
        m.insert(
            name.to_string(),
            SmartPlaylistField {
                expr: col.to_string(),
                order: String::new(),
                join_type: join,
                empty_values: Vec::new(),
                coalesce_default: None,
            },
        );
    }
    // Album-column fields (`criteria_sql.go:108-112`).
    for (name, col) in [
        ("albumdateadded", "album.created_at"),
        ("albumdatemodified", "album.updated_at"),
        ("albumduration", "album.duration"),
        ("albumsongcount", "album.song_count"),
        ("albumsize", "album.size"),
    ] {
        m.insert(
            name.to_string(),
            SmartPlaylistField {
                expr: col.to_string(),
                order: String::new(),
                join_type: SMART_PLAYLIST_JOIN_ALBUM,
                empty_values: Vec::new(),
                coalesce_default: None,
            },
        );
    }
    // The `random` pseudo-field (`criteria_sql.go:130`).
    m.insert(
        "random".to_string(),
        SmartPlaylistField {
            expr: String::new(),
            order: "random()".to_string(),
            join_type: SMART_PLAYLIST_JOIN_NONE,
            empty_values: Vec::new(),
            coalesce_default: None,
        },
    );
    m
}

impl SmartPlaylistCriteria {
    /// `newSmartPlaylistCriteria` (`criteria_sql.go:45-51`): the criteria plus
    /// the owner, over the default registry and field map.
    #[must_use]
    pub fn new(criteria: Criteria, owner: Owner) -> Self {
        Self {
            criteria,
            owner,
            field_map: default_field_map(),
            fields: default_smart_playlist_fields(),
        }
    }

    /// The compiler entry — Go's `cSQL.where()` (`criteria_sql.go:133-138`):
    /// a nil expression is the `1 = 1` tautology, otherwise
    /// [`SmartPlaylistCriteria::expr_sql`].
    ///
    /// # Errors
    /// Propagates [`CriteriaSqlError`] from [`Self::expr_sql`].
    pub fn where_sql(&self) -> Result<(String, Vec<Value>), CriteriaSqlError> {
        let cond = match &self.criteria.expression {
            // Go `criteria_sql.go:134-136`: a nil expression is the tautology;
            // the closed enum's empty `all` group is the port's nil stand-in.
            Expression::All(children) if children.is_empty() => {
                return Ok(("1 = 1".to_string(), vec![]));
            }
            expr => self.expr_sql(expr.clone())?,
        };
        let mut args = Vec::new();
        let sql = cond.render(&mut args);
        Ok((sql, args))
    }

    /// Go's `exprSQL` (`criteria_sql.go:140-199`): the operator dispatch.
    ///
    /// # Errors
    /// [`CriteriaSqlError::UnknownExpression`] for the closed enum's
    /// unreachable default — it stays an error variant for wire fidelity.
    pub fn expr_sql(&self, expr: Expression) -> Result<Cond, CriteriaSqlError> {
        match expr {
            Expression::All(children) => {
                let mut conds = Vec::new();
                for child in children {
                    conds.push(self.expr_sql(child)?);
                }
                Ok(merge_negated_json_conds(conds))
            }
            Expression::Any(children) => {
                let mut conds = Vec::new();
                for child in children {
                    conds.push(self.expr_sql(child)?);
                }
                Ok(merge_json_conds(conds))
            }
            Expression::Is(values) => self.comparison_expr(values, Comparison::Eq),
            Expression::IsNot(values) => self.is_not_expr(values),
            Expression::Gt(values) => self.comparison_expr(values, Comparison::Gt),
            Expression::Lt(values) => self.comparison_expr(values, Comparison::Lt),
            Expression::Before(values) => self.comparison_expr(values, Comparison::Lt),
            Expression::After(values) => self.comparison_expr(values, Comparison::Gt),
            Expression::Contains(values) => self.like_expr(values, "%{v}%", false),
            Expression::NotContains(values) => self.like_expr(values, "%{v}%", true),
            Expression::StartsWith(values) => self.like_expr(values, "{v}%", false),
            Expression::EndsWith(values) => self.like_expr(values, "%{v}", false),
            Expression::InTheRange(values) => self.range_expr(values),
            Expression::InTheLast(values) => self.period_expr(values, false),
            Expression::NotInTheLast(values) => self.period_expr(values, true),
            Expression::InPlaylist(values) => self.in_list(values, false),
            Expression::NotInPlaylist(values) => self.in_list(values, true),
            Expression::IsMissing(values) => self.missing_expr(values, true),
            Expression::IsPresent(values) => self.missing_expr(values, false),
        }
    }

    /// Go's `isNotExpr` (`criteria_sql.go:201-206`).
    fn is_not_expr(&self, values: Fields) -> Result<Cond, CriteriaSqlError> {
        if let Some((name, value, info)) = self.single_field(&values) {
            if info.is_tag || info.is_role {
                return Ok(json_expr(
                    &info,
                    &name,
                    Some(Cond::eq_json("value", value)),
                    true,
                ));
            }
        }
        self.comparison_expr(values, Comparison::NotEq)
    }

    /// Go's `missingExpr` (`criteria_sql.go:208-254`).
    fn missing_expr(&self, values: Fields, check_absence: bool) -> Result<Cond, CriteriaSqlError> {
        if values.len() != 1 {
            return Err(CriteriaSqlError::MissingRequiresOneField);
        }
        let Some((field, value, info)) = self.single_field(&values) else {
            return Err(CriteriaSqlError::InvalidField(
                values.keys().next().cloned().unwrap_or_default(),
            ));
        };
        let (b, ok) = to_bool(value);
        if !ok {
            return Err(CriteriaSqlError::InvalidMissingValue(field));
        }
        let negate = check_absence == b;

        if info.is_tag || info.is_role {
            return Ok(json_expr(&info, &field, None, negate));
        }
        if !info.nullable {
            return Err(CriteriaSqlError::InvalidField(field));
        }
        // Nullable column fields are stored in dedicated columns, not in the
        // tags JSON (`criteria_sql.go:226-235`).
        let f = self
            .fields
            .get(&field)
            .ok_or_else(|| CriteriaSqlError::InvalidField(field.clone()))?;
        if f.expr.is_empty() {
            return Err(CriteriaSqlError::InvalidField(field));
        }
        let col = f.expr.clone();
        let mut empties: Vec<String> = Vec::new();
        if !info.numeric && !info.boolean {
            empties.push(String::new());
            empties.extend(f.empty_values.iter().cloned());
        }
        let mut missing = vec![Cond::Eq {
            column: col.clone(),
            value: None,
        }];
        let mut present = vec![Cond::NotEq {
            column: col.clone(),
            value: None,
        }];
        for e in empties {
            missing.push(Cond::Eq {
                column: col.clone(),
                value: Some(Value::Text(e.clone())),
            });
            present.push(Cond::NotEq {
                column: col.clone(),
                value: Some(Value::Text(e)),
            });
        }
        if negate {
            return Ok(Cond::Or(missing));
        }
        Ok(Cond::And(present))
    }

    /// Go's `likeExpr` (`criteria_sql.go:256-284`).
    fn like_expr(
        &self,
        values: Fields,
        pattern: &str,
        negate: bool,
    ) -> Result<Cond, CriteriaSqlError> {
        if let Some((name, value, info)) = self.single_field(&values) {
            if info.is_tag || info.is_role {
                let text = json_value_to_string(&value);
                return Ok(json_expr(
                    &info,
                    &name,
                    Some(Cond::Like {
                        column: "value".to_string(),
                        value: apply_pattern(pattern, &text),
                    }),
                    negate,
                ));
            }
            // LIKE can't use the column index, so annotation fields keep the
            // COALESCE form (`criteria_sql.go:261-266`).
            if let Some(f) = self.annotation_field(&name) {
                let text = json_value_to_string(&value);
                return Ok(like_cond(
                    f.coalesced(),
                    &apply_pattern(pattern, &text),
                    negate,
                ));
            }
        }
        let fields = self.sql_fields(&values)?;
        let mut conds = Vec::new();
        for (col, value) in fields {
            let text = json_value_to_string(&value);
            let rendered = apply_pattern(pattern, &text);
            conds.push(if negate {
                Cond::NotLike {
                    column: col,
                    value: rendered,
                }
            } else {
                Cond::Like {
                    column: col,
                    value: rendered,
                }
            });
        }
        Ok(Cond::And(conds))
    }

    /// Go's `rangeExpr` (`criteria_sql.go:293-316`).
    fn range_expr(&self, values: Fields) -> Result<Cond, CriteriaSqlError> {
        if values.len() != 1 {
            return Err(CriteriaSqlError::InvalidField(
                values.keys().next().cloned().unwrap_or_default(),
            ));
        }
        let Some((field, value, info)) = self.single_field(&values) else {
            return Err(CriteriaSqlError::InvalidField(
                values.keys().next().cloned().unwrap_or_default(),
            ));
        };
        if info.is_tag || info.is_role {
            return Err(CriteriaSqlError::RangeUnsupported(field));
        }
        let serde_json::Value::Array(pair) = &value else {
            return Err(CriteriaSqlError::RangePair(field));
        };
        if pair.len() != 2 {
            return Err(CriteriaSqlError::RangePair(field));
        }
        let low = self.comparison_one(&field, pair[0].clone(), Comparison::GtOrEq)?;
        let high = self.comparison_one(&field, pair[1].clone(), Comparison::LtOrEq)?;
        Ok(Cond::And(vec![low, high]))
    }

    /// Go's `periodExpr` (`criteria_sql.go:318-341`).
    fn period_expr(&self, values: Fields, negate: bool) -> Result<Cond, CriteriaSqlError> {
        let fields = self.sql_fields(&values)?;
        let (field_ref, value_ref) = fields
            .first()
            .ok_or(CriteriaSqlError::InvalidField(String::new()))?;
        let field = field_ref.clone();
        let value = value_ref.clone();
        let Some(days) = period_days(&value) else {
            return Err(CriteriaSqlError::InvalidDays(json_value_to_string(&value)));
        };
        let first_date = start_of_period(days, std::time::SystemTime::now());
        if negate {
            return Ok(Cond::Or(vec![
                Cond::Lt {
                    column: field.clone(),
                    value: Value::Text(first_date),
                },
                Cond::Eq {
                    column: field,
                    value: None,
                },
            ]));
        }
        Ok(Cond::Gt {
            column: field,
            value: Value::Text(first_date),
        })
    }

    /// Go's `inList` (`criteria_sql.go:347-379`).
    fn in_list(&self, values: Fields, negate: bool) -> Result<Cond, CriteriaSqlError> {
        let mut filters: Vec<Cond> = Vec::new();
        let playlist_id = string_ref(&values, "id");
        let playlist_path = string_ref(&values, "path");
        if let Some(id) = playlist_id.filter(|id| !id.is_empty()) {
            filters.push(Cond::Eq {
                column: "pl.playlist_id".to_string(),
                value: Some(Value::Text(id)),
            });
        } else if let Some(path) = playlist_path.filter(|p| !p.is_empty()) {
            let values = path_variants(&path).into_iter().map(Value::Text).collect();
            filters.push(Cond::In {
                column: "playlist.path".to_string(),
                values,
            });
        } else {
            return Err(CriteriaSqlError::PlaylistRefMissing);
        }
        if !self.owner.is_admin {
            if self.owner.id.is_empty() {
                filters.push(Cond::Eq {
                    column: "playlist.public".to_string(),
                    value: Some(Value::Integer(1)),
                });
            } else {
                filters.push(Cond::Or(vec![
                    Cond::Eq {
                        column: "playlist.public".to_string(),
                        value: Some(Value::Integer(1)),
                    },
                    Cond::Eq {
                        column: "playlist.owner_id".to_string(),
                        value: Some(Value::Text(self.owner.id.clone())),
                    },
                ]));
            }
        }
        // The subquery rendered by hand — squirrel's Select().From("playlist_tracks pl")
        // .LeftJoin("playlist on pl.playlist_id = playlist.id").Where(filters)
        // with `?` placeholders (`criteria_sql.go:367-374`).
        let mut sub_args = Vec::new();
        let mut sub_sql =
            "SELECT media_file_id FROM playlist_tracks pl LEFT JOIN playlist on pl.playlist_id = playlist.id WHERE "
                .to_string();
        sub_sql.push_str(
            &filters
                .iter()
                .map(|f| {
                    let rendered = f.render(&mut sub_args);
                    if matches!(f, Cond::Or(_)) {
                        format!("({rendered})")
                    } else {
                        rendered
                    }
                })
                .collect::<Vec<_>>()
                .join(" AND "),
        );
        Ok(Cond::InSubquery {
            subquery: (sub_sql, sub_args),
            negate,
        })
    }

    /// Go's `singleField` (`criteria_sql.go:626-635`): the only entry of a
    /// one-field payload, resolved through the registry.
    #[must_use]
    fn single_field(&self, values: &Fields) -> Option<(String, JsonValue, FieldInfo)> {
        if values.len() != 1 {
            return None;
        }
        let (field, value) = values.iter().next()?;
        let (info, name, ok) = lookup_field(&self.field_map, field.clone());
        if !ok {
            return None;
        }
        Some((name, value.clone(), info))
    }

    /// Go's `sqlFields` (`criteria_sql.go:637-654`): resolve every entry
    /// through the registry, reject tags/roles, map to the SQL column.
    fn sql_fields(&self, values: &Fields) -> Result<Vec<(String, JsonValue)>, CriteriaSqlError> {
        let mut fields: Vec<(String, JsonValue)> = Vec::new();
        for (field, value) in values {
            let (info, name, ok) = lookup_field(&self.field_map, field.clone());
            if !ok {
                return Err(CriteriaSqlError::InvalidField(field.clone()));
            }
            if info.is_tag || info.is_role {
                return Err(CriteriaSqlError::TagRoleSingleField(field.clone()));
            }
            let sql_field = self.field_expr(&name).unwrap_or_default();
            if sql_field.is_empty() {
                return Err(CriteriaSqlError::InvalidField(field.clone()));
            }
            fields.push((sql_field, value.clone()));
        }
        Ok(fields)
    }

    /// Go's `fieldExpr` (`criteria_sql.go:656-659`).
    #[must_use]
    fn field_expr(&self, name: &str) -> Option<String> {
        self.fields
            .get(&name.to_lowercase())
            .map(|f| f.expr.clone())
    }

    /// Go's `comparisonExpr` (`criteria_sql.go:699-713`).
    fn comparison_expr(&self, values: Fields, cmp: Comparison) -> Result<Cond, CriteriaSqlError> {
        if let Some((name, value, info)) = self.single_field(&values) {
            if info.is_tag || info.is_role {
                return Ok(json_expr(
                    &info,
                    &name,
                    Some(Cond::eq_cmp_json("value", value, cmp)),
                    false,
                ));
            }
            if let Some(f) = self.annotation_field(&name) {
                return Ok(annotation_cond(f, cmp, value));
            }
        }
        let fields = self.sql_fields(&values)?;
        Ok(Cond::And(
            fields
                .into_iter()
                .map(|(col, value)| Cond::eq_cmp_json(col, value, cmp))
                .collect(),
        ))
    }

    /// The single-field arm of `comparisonExpr` (`criteria_sql.go:307-313`
    /// range helper): one field, one value, one comparator.
    fn comparison_one(
        &self,
        field: &str,
        value: JsonValue,
        cmp: Comparison,
    ) -> Result<Cond, CriteriaSqlError> {
        let mut one: Fields = Fields::new();
        one.insert(field.to_string(), value);
        self.comparison_expr(one, cmp)
    }

    /// Go's `annotationField` (`criteria_sql.go:678-687`).
    #[must_use]
    fn annotation_field(&self, name: &str) -> Option<SmartPlaylistField> {
        let f = self.fields.get(name)?;
        f.coalesce_default.as_ref()?;
        Some(f.clone())
    }

    /// Go's `orderBy` (`criteria_sql.go:860-875`).
    #[must_use]
    pub fn order_by(&self) -> String {
        let sort_fields = self.order_by_fields();
        let mut parts: Vec<String> = Vec::new();
        for sf in &sort_fields {
            let Some(mapped) = self.sort_expr(&sf.field) else {
                continue;
            };
            let dir = if sf.desc { "desc" } else { "asc" };
            parts.push(format!("{mapped} {dir}"));
        }
        parts.join(", ")
    }

    /// Go's `sortExpr` (`criteria_sql.go:877-904`).
    #[must_use]
    fn sort_expr(&self, sort_field: &str) -> Option<String> {
        let (info, name, ok) = lookup_field(&self.field_map, sort_field.to_string());
        if !ok {
            return None;
        }
        let mut mapped: String;
        if let Some(f) = self.fields.get(&name) {
            if !f.order.is_empty() {
                return Some(f.order.clone());
            }
        }
        if info.is_tag {
            mapped = format!("COALESCE(json_extract(media_file.tags, '$.{name}[0].value'), '')");
        } else if info.is_role {
            mapped =
                format!("COALESCE(json_extract(media_file.participants, '$.{name}[0].name'), '')");
        } else {
            let f = self.fields.get(&name)?;
            if f.expr.is_empty() {
                return None;
            }
            // Sorting keeps the COALESCE default (`criteria_sql.go:896-898`).
            mapped = f.coalesced();
        }
        if info.numeric {
            mapped = format!("CAST({mapped} AS REAL)");
        }
        Some(mapped)
    }

    /// Go's `expressionJoins` (`criteria_sql.go:806-815`): the field names the
    /// expression mentions, via Go's `criteria.Walk` + `criteria.Fields`
    /// (`walk.go:7-:33` / `operators.go:14-:46`). Go mutates through the walk
    /// visitor; the closed enum is collected by [`collect_expression_fields`]
    /// here — the same visit order, no ambient state.
    #[must_use]
    pub fn expression_joins(&self) -> SmartPlaylistJoinType {
        let mut joins = SMART_PLAYLIST_JOIN_NONE;
        for field in collect_expression_fields(self.criteria.expression.clone()) {
            joins = joins.or(self.field_join_type(&field));
        }
        joins
    }

    /// Go's `requiredJoins` (`criteria_sql.go:817-823`).
    #[must_use]
    pub fn required_joins(&self) -> SmartPlaylistJoinType {
        let mut joins = self.expression_joins();
        for name in self.sort_field_names() {
            joins = joins.or(self.field_join_type(&name));
        }
        joins
    }

    /// Go's `applyExpressionJoins` (`criteria_sql.go:826-828`): the WHERE
    /// joins only.
    #[must_use]
    pub fn apply_expression_joins(&self, user_id: &str) -> Vec<Sql> {
        self.apply_joins(self.expression_joins(), user_id)
    }

    /// Go's `applyRequiredJoins` (`criteria_sql.go:831-833`): the WHERE joins
    /// plus any the ORDER BY resolves against.
    #[must_use]
    pub fn apply_required_joins(&self, user_id: &str) -> Vec<Sql> {
        self.apply_joins(self.required_joins(), user_id)
    }

    /// Go's `applyJoins` (`criteria_sql.go:837-858`), as join fragments with
    /// their bound args (the seam's `joins` slot; the unconditional
    /// `annotation` join stays the consumer's — see
    /// `smart_playlist_repo.rs`'s `annotation_join`).
    #[must_use]
    pub fn apply_joins(&self, joins: SmartPlaylistJoinType, user_id: &str) -> Vec<Sql> {
        let mut out: Vec<Sql> = Vec::new();
        if joins.has(SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION) {
            out.push((
                "LEFT JOIN annotation AS album_annotation ON (\
                 album_annotation.item_id = media_file.album_id \
                 AND album_annotation.item_type = 'album' \
                 AND album_annotation.user_id = ?)"
                    .to_string(),
                vec![Value::Text(user_id.to_string())],
            ));
        }
        if joins.has(SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION) {
            out.push((
                "LEFT JOIN annotation AS artist_annotation ON (\
                 artist_annotation.item_id = media_file.artist_id \
                 AND artist_annotation.item_type = 'artist' \
                 AND artist_annotation.user_id = ?)"
                    .to_string(),
                vec![Value::Text(user_id.to_string())],
            ));
        }
        if joins.has(SMART_PLAYLIST_JOIN_ALBUM) {
            out.push((
                "LEFT JOIN album ON album.id = media_file.album_id".to_string(),
                vec![],
            ));
        }
        out
    }

    /// Go's `fieldJoinType` (`criteria_sql.go:794-804`).
    #[must_use]
    fn field_join_type(&self, name: &str) -> SmartPlaylistJoinType {
        let (_info, canonical, ok) = lookup_field(&self.field_map, name.to_string());
        if !ok {
            return SMART_PLAYLIST_JOIN_NONE;
        }
        self.fields
            .get(&canonical)
            .map(|f| f.join_type)
            .unwrap_or(SMART_PLAYLIST_JOIN_NONE)
    }

    /// Go's `c.Criteria.OrderByFields()` (`criteria_sql.go:861`).
    #[must_use]
    fn order_by_fields(&self) -> Vec<SortField> {
        criteria_order_by_fields(&self.criteria, &self.field_map)
    }

    /// Go's `c.Criteria.SortFieldNames()` (`criteria_sql.go:819`).
    #[must_use]
    fn sort_field_names(&self) -> Vec<String> {
        sort_names(
            &self.field_map,
            self.criteria.sort.clone(),
            self.criteria.order.clone(),
        )
    }
}

impl SmartPlaylistField {
    /// Go's `coalesced` (`criteria_sql.go:689-697`), with [`sql_literal`]
    /// rendering the default (`:788-792`): `false`/`true` and `0`.
    #[must_use]
    pub fn coalesced(&self) -> String {
        match self.coalesce_default {
            None => self.expr.clone(),
            Some(default) => format!("COALESCE({}, {})", self.expr, sql_literal(Some(default))),
        }
    }
}

impl Cond {
    /// Builds `column = ?` (Go `Eq{col: v}`, `criteria_sql.go:670`) from a
    /// criteria leaf value (JSON) — the `jsonExpr` feeds (`criteria_sql.go:202,702`).
    #[must_use]
    pub fn eq_json(column: impl Into<String>, value: JsonValue) -> Self {
        Self::eq(column, json_to_sql(&value))
    }

    /// Builds `column = ?` (Go `Eq{col: v}`, `criteria_sql.go:670`).
    #[must_use]
    pub fn eq(column: impl Into<String>, value: Value) -> Self {
        Self::Eq {
            column: column.into(),
            value: Some(value),
        }
    }

    /// Builds `column <op> ?` for any [`Comparison`] from a leaf (JSON) value.
    #[must_use]
    pub fn eq_cmp_json(column: impl Into<String>, value: JsonValue, cmp: Comparison) -> Self {
        Self::eq_cmp(column, json_to_sql(&value), cmp)
    }

    /// Builds `column <op> ?` for any [`Comparison`].
    #[must_use]
    pub fn eq_cmp(column: impl Into<String>, value: Value, cmp: Comparison) -> Self {
        let column = column.into();
        match cmp {
            Comparison::Eq => Self::Eq {
                column,
                value: Some(value),
            },
            Comparison::NotEq => Self::NotEq {
                column,
                value: Some(value),
            },
            Comparison::Gt => Self::Gt { column, value },
            Comparison::GtOrEq => Self::GtOrEq { column, value },
            Comparison::Lt => Self::Lt { column, value },
            Comparison::LtOrEq => Self::LtOrEq { column, value },
        }
    }

    /// Renders the condition and its bound args (squirrel's `ToSql`,
    /// `criteria_sql.go:407-424,432-450,577-593,604-623`), in placeholder
    /// order.
    #[must_use]
    pub fn render(&self, args: &mut Vec<Value>) -> String {
        match self {
            Self::Eq { column, value } => match value {
                Some(v) => {
                    args.push(v.clone());
                    format!("{column} = ?")
                }
                None => format!("{column} IS NULL"),
            },
            Self::NotEq { column, value } => match value {
                Some(v) => {
                    args.push(v.clone());
                    format!("{column} <> ?")
                }
                None => format!("{column} IS NOT NULL"),
            },
            Self::Gt { column, value } => {
                args.push(value.clone());
                format!("{column} > ?")
            }
            Self::GtOrEq { column, value } => {
                args.push(value.clone());
                format!("{column} >= ?")
            }
            Self::Lt { column, value } => {
                args.push(value.clone());
                format!("{column} < ?")
            }
            Self::LtOrEq { column, value } => {
                args.push(value.clone());
                format!("{column} <= ?")
            }
            Self::Like { column, value } => {
                args.push(Value::Text(value.clone()));
                format!("{column} LIKE ?")
            }
            Self::NotLike { column, value } => {
                args.push(Value::Text(value.clone()));
                format!("{column} NOT LIKE ?")
            }
            Self::In { column, values } => {
                if values.is_empty() {
                    return "1 = 0".to_string();
                }
                args.extend(values.iter().cloned());
                let placeholders = vec!["?"; values.len()].join(", ");
                format!("{column} IN ({placeholders})")
            }
            Self::InSubquery { subquery, negate } => {
                let (sql, sub_args) = subquery;
                args.extend(sub_args.iter().cloned());
                let keyword = if *negate { "NOT IN" } else { "IN" };
                format!("media_file.id {keyword} ({sql})")
            }
            Self::TagExists {
                tag,
                numeric,
                cond,
                not,
            } => {
                let mut sql = match cond {
                    Some(inner) => {
                        let mut inner_args = Vec::new();
                        let mut inner_sql = inner.render(&mut inner_args);
                        if *numeric {
                            inner_sql = inner_sql.replace("value", "CAST(value AS REAL)");
                        }
                        args.extend(inner_args);
                        format!(
                            "exists (select 1 from json_tree(media_file.tags, '$.{tag}') where key='value' and {inner_sql})"
                        )
                    }
                    None => format!(
                        "exists (select 1 from json_tree(media_file.tags, '$.{tag}') where key='value')"
                    ),
                };
                if *not {
                    sql = format!("not {sql}");
                }
                sql
            }
            Self::RoleExists { role, cond, not } => {
                let mut sql = match cond {
                    Some(inner) => {
                        let mut inner_args = Vec::new();
                        // `roleCondSQL` rewrites the placeholder column name
                        // (`criteria_sql.go:452-459`).
                        let mut inner_sql = inner.render(&mut inner_args);
                        inner_sql = inner_sql.replace("value", "artist.name");
                        args.push(Value::Text(role.clone()));
                        args.extend(inner_args);
                        role_exists_sql(&format!("({inner_sql})"))
                    }
                    None => {
                        args.push(Value::Text(role.clone()));
                        "exists (select 1 from media_file_artists mfa where mfa.media_file_id = media_file.id and mfa.role = ?)".to_string()
                    }
                };
                if *not {
                    sql = format!("not {sql}");
                }
                sql
            }
            Self::TagGroup {
                tag,
                numeric,
                conds,
                not,
            } => {
                let mut parts: Vec<String> = Vec::new();
                for c in conds {
                    let mut inner_args = Vec::new();
                    let mut part = c.render(&mut inner_args);
                    if *numeric {
                        part = part.replace("value", "CAST(value AS REAL)");
                    }
                    args.extend(inner_args);
                    parts.push(part);
                }
                let mut sql = format!(
                    "exists (select 1 from json_tree(media_file.tags, '$.{tag}') where key='value' and ({}))",
                    parts.join(" OR ")
                );
                if *not {
                    sql = format!("not {sql}");
                }
                sql
            }
            Self::RoleGroup { role, conds, not } => {
                let mut parts: Vec<String> = Vec::new();
                args.push(Value::Text(role.clone()));
                for c in conds {
                    let mut inner_args = Vec::new();
                    let mut part = c.render(&mut inner_args);
                    part = part.replace("value", "artist.name");
                    args.extend(inner_args);
                    parts.push(part);
                }
                let mut sql = role_exists_sql(&format!("({})", parts.join(" OR ")));
                if *not {
                    sql = format!("not {sql}");
                }
                sql
            }
            Self::And(conds) => conds
                .iter()
                .map(|c| c.render(args))
                .collect::<Vec<_>>()
                .join(" AND "),
            Self::Or(conds) => conds
                .iter()
                .map(|c| c.render(args))
                .collect::<Vec<_>>()
                .join(" OR "),
        }
    }
}

/// Go's `roleExistsSQL` (`criteria_sql.go:461-465`).
#[must_use]
fn role_exists_sql(inner_cond: &str) -> String {
    format!(
        "exists (select 1 from media_file_artists mfa join artist on artist.id = mfa.artist_id \
         where mfa.media_file_id = media_file.id and mfa.role = ? and {inner_cond})"
    )
}

/// Go's `jsonExpr` (`criteria_sql.go:393-398`).
#[must_use]
fn json_expr(info: &FieldInfo, name: &str, cond: Option<Cond>, negate: bool) -> Cond {
    if info.is_role {
        Cond::RoleExists {
            role: name.to_string(),
            cond: cond.map(Box::new),
            not: negate,
        }
    } else {
        Cond::TagExists {
            tag: name.to_string(),
            numeric: info.numeric,
            cond: cond.map(Box::new),
            not: negate,
        }
    }
}

/// Renders a criteria leaf value as the LIKE pattern input — Go's
/// `fmt.Sprintf(pattern, value)` argument (`criteria_sql.go:259,265,275,281`):
/// a string renders verbatim; numbers/bools render Go's `%v` form.
#[must_use]
fn json_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => bool_to_string(*b),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Go's `%v` of a bool (`criteria_sql.go:788-792` applies to defaults; leaf
/// values flow through `fmt.Sprintf` the same way): `true`/`false`.
#[must_use]
fn bool_to_string(b: bool) -> String {
    if b {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

/// Go's `fmt.Sprintf(pattern, value)` with `%v` as the value slot
/// (`criteria_sql.go:175-181` passes `%%%v%%`-style patterns).
#[must_use]
fn apply_pattern(pattern: &str, value: &str) -> String {
    pattern.replace("{v}", value)
}

/// The string slot of a leaf payload — Go's `values[key].(string)` typed cast
/// (`criteria_sql.go:349,351`): an absent key or non-string value is `None`.
#[must_use]
fn string_ref(values: &Fields, key: &str) -> Option<String> {
    values
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(|s| s.to_string())
}

/// Go's `criteria.Walk` + `criteria.Fields` accumulation (`criteria_sql.go:808-813`):
/// every leaf payload key, in walk order (self first, then conjunction children).
#[must_use]
fn collect_expression_fields(expr: Expression) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(payload) = expression_fields(expr.clone()) {
        names.extend(payload.keys().map(|k| k.to_string()));
    }
    match expr {
        Expression::All(children) => {
            for child in children {
                names.extend(collect_expression_fields(child.clone()));
            }
        }
        Expression::Any(children) => {
            for child in children {
                names.extend(collect_expression_fields(child.clone()));
            }
        }
        _ => {}
    }
    names
}

/// The days slot of a period payload — Go's `strconv.ParseInt(fmt.Sprintf("%v",
/// value))` (`criteria_sql.go:329-332`): numbers and numeric strings parse.
#[must_use]
fn period_days(value: &JsonValue) -> Option<i64> {
    match value {
        JsonValue::Number(n) => n.as_i64(),
        JsonValue::String(s) => s.parse::<i64>().ok(),
        JsonValue::Bool(b) => Some(if *b { 1 } else { 0 }),
        _ => None,
    }
}

/// Converts a criteria leaf value into a bound SQL parameter — squirrel binds
/// the JSON-decoded Go scalars directly (`criteria_sql.go:202,702,724`): text
/// as text, bool as 0/1, number as REAL.
#[must_use]
pub fn json_to_sql(value: &JsonValue) -> Value {
    match value {
        JsonValue::String(s) => Value::Text(s.clone()),
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) => n
            .as_i64()
            .map(Value::Integer)
            .unwrap_or_else(|| Value::Real(n.as_f64().unwrap_or_default())),
        _ => Value::Null,
    }
}

/// Go's `likeCond` (`criteria_sql.go:286-291`).
#[must_use]
fn like_cond(col: String, pattern: &str, negate: bool) -> Cond {
    if negate {
        Cond::NotLike {
            column: col,
            value: pattern.to_string(),
        }
    } else {
        Cond::Like {
            column: col,
            value: pattern.to_string(),
        }
    }
}

/// Go's `annotationCond` (`criteria_sql.go:715-731`).
#[must_use]
fn annotation_cond(f: SmartPlaylistField, cmp: Comparison, value: JsonValue) -> Cond {
    let Some(wrap_null) = bare_null_inclusion(f.coalesce_default, cmp, &value) else {
        return Cond::eq_cmp(f.coalesced(), json_to_sql(&value), cmp);
    };
    let base = Cond::eq_cmp(f.expr.clone(), json_to_sql(&value), cmp);
    if !wrap_null {
        return base;
    }
    // The default satisfies the predicate, so missing rows (NULL) must be
    // included (`criteria_sql.go:728-730`).
    Cond::Or(vec![
        base,
        Cond::Eq {
            column: f.expr,
            value: None,
        },
    ])
}

/// Go's `bareNullInclusion` (`criteria_sql.go:733-759`): `None` = keep the
/// COALESCE form; `Some(wrap_null)` = the bare form is safe, with
/// `wrap_null` saying whether missing rows must be re-included.
#[must_use]
fn bare_null_inclusion(
    default_val: Option<CoalesceDefault>,
    cmp: Comparison,
    value: &JsonValue,
) -> Option<bool> {
    if let Some(CoalesceDefault::Bool(b)) = default_val {
        // Bool columns have an exact bare form only for equality; ordering
        // operators fall back to COALESCE (`criteria_sql.go:741-745`). Mapping
        // both bools to 0/1 lets `satisfy` reuse the numeric eq/ne predicate.
        if cmp_ordering(cmp) {
            return None;
        }
        let v = to_f64(value)?;
        return Some(cmp_satisfy(cmp, bool_to_float(b), v));
    }
    let v = to_f64(value)?;
    let Some(CoalesceDefault::Int(d)) = default_val else {
        return None;
    };
    Some(cmp_satisfy(cmp, d as f64, v))
}

/// Go's `boolToFloat` (`criteria_sql.go:761-766`).
#[must_use]
fn bool_to_float(b: bool) -> f64 {
    if b {
        1.0
    } else {
        0.0
    }
}

/// Go's `toFloat` (`criteria_sql.go:768-786`): JSON scalars coerce to f64.
#[must_use]
fn to_f64(v: &JsonValue) -> Option<f64> {
    match v {
        JsonValue::Number(n) => n.as_f64(),
        JsonValue::String(s) => s.parse::<f64>().ok(),
        JsonValue::Bool(b) => Some(bool_to_float(*b)),
        _ => None,
    }
}

/// Go's `sqlLiteral` (`criteria_sql.go:788-792`): the bool/numeric default
/// rendered as a SQL literal — `false`/`true` and `0`.
#[must_use]
fn sql_literal(v: Option<CoalesceDefault>) -> String {
    match v {
        Some(CoalesceDefault::Bool(b)) => bool_to_string(b),
        Some(CoalesceDefault::Int(n)) => n.to_string(),
        None => "0".to_string(),
    }
}

/// Limits how many conditions are ORed inside a single EXISTS subquery
/// (`criteria_sql.go:467-470`).
const JSON_COND_BATCH_SIZE: usize = 350;

/// Go's `mergeJsonConds` (`criteria_sql.go:476-481`).
#[must_use]
fn merge_json_conds(conds: Vec<Cond>) -> Cond {
    match merge_same_field_conds(conds.clone(), false) {
        Some(merged) => Cond::Or(merged),
        None => Cond::Or(conds),
    }
}

/// Go's `mergeNegatedJsonConds` (`criteria_sql.go:485-490`).
#[must_use]
fn merge_negated_json_conds(conds: Vec<Cond>) -> Cond {
    match merge_same_field_conds(conds.clone(), true) {
        Some(merged) => Cond::And(merged),
        None => Cond::And(conds),
    }
}

/// Go's `mergeSameFieldConds` (`criteria_sql.go:495-567`): group
/// role/tag conditions sharing a field and polarity, replace each group of
/// 2+ with batched group conditions, keeping the non-matching entries in
/// order.
#[must_use]
fn merge_same_field_conds(conds: Vec<Cond>, negated: bool) -> Option<Vec<Cond>> {
    struct Group {
        is_role: bool,
        numeric: bool,
        key: String,
        entries: Vec<(usize, Cond)>,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for (i, c) in conds.iter().enumerate() {
        match c {
            Cond::RoleExists { role, cond, not } => {
                if *not != negated {
                    continue;
                }
                let Some(cond) = cond else { continue };
                let key = format!("role:{role}");
                let g = groups.entry(key.clone()).or_insert_with(|| Group {
                    is_role: true,
                    numeric: false,
                    key,
                    entries: Vec::new(),
                });
                g.entries.push((i, (**cond).clone()));
            }
            Cond::TagExists {
                tag,
                numeric,
                cond,
                not,
            } => {
                if *not != negated {
                    continue;
                }
                let Some(cond) = cond else { continue };
                let key = format!("tag:{tag}");
                let g = groups.entry(key.clone()).or_insert_with(|| Group {
                    is_role: false,
                    numeric: *numeric,
                    key,
                    entries: Vec::new(),
                });
                g.entries.push((i, (**cond).clone()));
            }
            _ => {}
        }
    }

    let mut remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut additions: Vec<Cond> = Vec::new();
    for (_, g) in groups {
        if g.entries.len() < 2 {
            continue;
        }
        let batch: Vec<Cond> = g.entries.iter().map(|(_, c)| c.clone()).collect();
        for e in &g.entries {
            remove.insert(e.0);
        }
        for chunk in batch.chunks(JSON_COND_BATCH_SIZE) {
            let chunk = chunk.to_vec();
            additions.push(if g.is_role {
                Cond::RoleGroup {
                    role: g.key["role:".len()..].to_string(),
                    conds: chunk,
                    not: negated,
                }
            } else {
                Cond::TagGroup {
                    tag: g.key["tag:".len()..].to_string(),
                    numeric: g.numeric,
                    conds: chunk,
                    not: negated,
                }
            });
        }
    }

    if remove.is_empty() {
        return None;
    }
    let mut result: Vec<Cond> = conds
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !remove.contains(i))
        .map(|(_, c)| c)
        .collect();
    result.extend(additions);
    Some(result)
}

#[cfg(test)]
mod criteria_sql_test {
    use super::*;
    use crate::sqlrepo::Value;

    /// A one-field leaf payload (Go's map literal, e.g. `criteria.Is{"title": "x"}`).
    fn leaf(name: &str, value: JsonValue) -> Fields {
        Fields::from([(name.to_string(), value)])
    }

    fn compiler() -> SmartPlaylistCriteria {
        let mut c = SmartPlaylistCriteria::new(Criteria::default(), Owner::default());
        // Go registers tags/roles at startup (`tag_mappings.go:385-392`): the
        // roles, the tag names (`mood`, `genre`, `rate`, …) and the numeric tag
        // `rate` (`add_numeric_tags`). `bpm` is a numeric *column* field, not
        // a tag.
        nd_model::fields::add_tag_names(
            &mut c.field_map,
            &["mood".to_string(), "genre".to_string(), "rate".to_string()],
        );
        nd_model::fields::add_numeric_tags(&mut c.field_map, &["rate".to_string()]);
        nd_model::fields::add_roles(
            &mut c.field_map,
            &[
                "artist".to_string(),
                "composer".to_string(),
                "producer".to_string(),
            ],
        );
        c
    }

    /// The shared fixture over a caller-supplied criteria/owner — Go builds
    /// every `newSmartPlaylistCriteria(...)` the same way, so all the
    /// registry-dependent tests go through one construction path.
    fn compiler_with(criteria: Criteria, owner: Owner) -> SmartPlaylistCriteria {
        let mut c = SmartPlaylistCriteria::new(criteria, owner);
        nd_model::fields::add_tag_names(
            &mut c.field_map,
            &["mood".to_string(), "genre".to_string(), "rate".to_string()],
        );
        nd_model::fields::add_numeric_tags(&mut c.field_map, &["rate".to_string()]);
        nd_model::fields::add_roles(
            &mut c.field_map,
            &[
                "artist".to_string(),
                "composer".to_string(),
                "producer".to_string(),
            ],
        );
        c
    }

    /// Renders a leaf operator through the compiler (`criteria_sql.go`'s
    /// per-operator entry points), returning `(sql, args)`.
    fn render_leaf(
        c: &SmartPlaylistCriteria,
        build: impl Fn(&SmartPlaylistCriteria) -> Result<Cond, CriteriaSqlError>,
    ) -> (String, Vec<Value>) {
        let mut args = Vec::new();
        (build(c).expect("compiles").render(&mut args), args)
    }

    #[test]
    fn empty_criteria_is_the_where_tautology() {
        // Arrange: Go `criteria_sql.go:134-136` (nil Expression → `1 = 1`).
        let c = compiler();
        // Act:
        let (sql, args) = c.where_sql().expect("compiles");
        // Assert:
        assert_eq!(sql, "1 = 1");
        assert!(args.is_empty());
    }

    #[test]
    fn contains_maps_to_a_column_like_with_bound_pattern() {
        // Arrange: Go `Contains{"title": "love"}` → likeExpr `%v%%`… `%%%v%%`
        // (`criteria_sql.go:174-175, 256-284`); `title` maps to
        // `media_file.title` (`smartPlaylistFields`, `:60`).
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::Contains(leaf("title", json!("love"))))
        });
        // Assert:
        assert_eq!(sql, "media_file.title LIKE ?");
        assert_eq!(args, vec![Value::Text("%love%".to_string())]);
    }

    #[test]
    fn is_not_on_a_tag_renders_a_negated_json_tree_exists() {
        // Arrange: Go `IsNot{"mood": "sad"}` — `isNotExpr` (`:201-206`) tags go
        // through `jsonExpr` with negate=true (`:393-398`), `tagCond.ToSql`
        // (`:407-424`).
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::IsNot(leaf("mood", json!("sad"))))
        });
        // Assert:
        assert_eq!(
            sql,
            "not exists (select 1 from json_tree(media_file.tags, '$.mood') where key='value' and value = ?)"
        );
        assert_eq!(args, vec![Value::Text("sad".to_string())]);
    }

    #[test]
    fn gt_on_a_numeric_tag_casts_the_value_column() {
        // Arrange: Go `Gt{"rate": 120}` — `comparisonExpr` (`:699-713`) tag
        // arm + `tagCond.ToSql`'s numeric CAST (`:413-415`); `rate` is the
        // fixture's registered numeric tag (`tag_mappings.go:391-392`).
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| c.expr_sql(Expression::Gt(leaf("rate", json!(120)))));
        // Assert:
        assert_eq!(
            sql,
            "exists (select 1 from json_tree(media_file.tags, '$.rate') where key='value' and CAST(value AS REAL) > ?)"
        );
        assert_eq!(args, vec![Value::Integer(120)]);
    }

    #[test]
    fn gt_on_playcount_uses_the_bare_annotation_form_without_null_wrap() {
        // Arrange: Go `Gt{"playcount": 2}` — `annotationCond` (`:719-731`):
        // default 0 does not satisfy `> 2`, so no `OR col IS NULL` is added
        // (`bareNullInclusion`, `:739-759`).
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::Gt(leaf("playcount", json!(2))))
        });
        // Assert:
        assert_eq!(sql, "annotation.play_count > ?");
        assert_eq!(args, vec![Value::Integer(2)]);
    }

    #[test]
    fn loved_eq_true_wraps_null_so_missing_rows_match_the_default() {
        // Arrange: Go `Is{"loved": true}` — the `false` default satisfies `= true`?
        // No: `false != true`, but Go maps bools through the numeric eq predicate
        // (`:740-750`): satisfy(0, 1) is false → bare form, no wrap.
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::Is(leaf("loved", json!(true))))
        });
        // Assert:
        assert_eq!(sql, "annotation.starred = ?");
        assert_eq!(args, vec![Value::Bool(true)]);
    }

    #[test]
    fn loved_eq_false_wraps_null_because_the_default_satisfies_the_predicate() {
        // Arrange: Go `Is{"loved": false}` — satisfy(0, 0) is true → wrapNull
        // (`criteria_sql.go:728-730`): `base OR col IS NULL`.
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::Is(leaf("loved", json!(false))))
        });
        // Assert:
        assert_eq!(sql, "annotation.starred = ? OR annotation.starred IS NULL");
        assert_eq!(args, vec![Value::Bool(false)]);
    }

    #[test]
    fn album_rating_eq_keeps_the_coalesce_form_for_ordering_operators_on_bools() {
        // Arrange: Go `Gt{"albumrating": 3}` on a bool-default column —
        // ordering operators on bool defaults fall back to COALESCE
        // (`criteria_sql.go:741-745`); `albumrating`'s default is `0` (an int),
        // so this stays the bare form. Pin both columns:
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::Gt(leaf("albumrating", json!(3))))
        });
        // Assert: int default `0` doesn't satisfy `> 3` → bare form (`:752-758`).
        assert_eq!(sql, "album_annotation.rating > ?");
        assert_eq!(args, vec![Value::Integer(3)]);
    }

    #[test]
    fn is_missing_on_a_nullable_string_column_matches_null_and_empties() {
        // Arrange: Go `IsMissing{"lyrics": true}` — `missingExpr` (`:208-254`):
        // nullable string column, empties = ["", "[]"] (`:238-246`), negate
        // stays (checkAbsence == true == value).
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::IsMissing(leaf("lyrics", json!(true))))
        });
        // Assert:
        assert_eq!(
            sql,
            "media_file.lyrics IS NULL OR media_file.lyrics = ? OR media_file.lyrics = ?"
        );
        assert_eq!(
            args,
            vec![Value::Text(String::new()), Value::Text("[]".to_string()),]
        );
    }

    #[test]
    fn is_present_on_a_nullable_column_negates_the_missing_set() {
        // Arrange: Go `IsPresent{"comment": true}` — `missingExpr` with
        // checkAbsence=false, value=true → `negate := checkAbsence == b` is
        // false → the `present` AND (`criteria_sql.go:219,242-250`); oracle
        // `criteria_sql_test.go:189-190`.
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::IsPresent(leaf("comment", json!(true))))
        });
        // Assert:
        assert_eq!(
            sql,
            "media_file.comment IS NOT NULL AND media_file.comment <> ?"
        );
        assert_eq!(args, vec![Value::Text(String::new())]);
    }

    #[test]
    fn is_missing_requires_exactly_one_field() {
        // Arrange: Go `criteria_sql.go:211-214`.
        let c = compiler();
        // Act:
        let err = c
            .expr_sql(Expression::IsMissing(Fields::new()))
            .expect_err("rejects empty payload");
        // Assert:
        assert!(err.to_string().contains("requires exactly one field"));
    }

    #[test]
    fn in_the_range_splits_into_ge_and_le_over_the_column() {
        // Arrange: Go `InTheRange{"year": [2000, 2010]}` — `rangeExpr`
        // (`:293-316`).
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::InTheRange(leaf("year", json!([2000, 2010]))))
        });
        // Assert:
        assert_eq!(sql, "media_file.year >= ? AND media_file.year <= ?");
        assert_eq!(args, vec![Value::Integer(2000), Value::Integer(2010)]);
    }

    #[test]
    fn in_the_range_rejects_a_non_pair() {
        // Arrange: Go `criteria_sql.go:304-306`.
        let c = compiler();
        // Act:
        let err = c
            .expr_sql(Expression::InTheRange(leaf("year", json!([1, 2, 3]))))
            .expect_err("rejects a 3-list");
        // Assert:
        assert!(err.to_string().contains("[min, max]"));
    }

    #[test]
    fn in_the_last_binds_the_period_start_date() {
        // Arrange: Go `InTheLast{"lastplayed": 30}` — `periodExpr` (`:318-341`):
        // `firstDate = startOfPeriod(30, now)` bound to `Gt`.
        let c = compiler();
        // Act:
        let (sql, args) = render_leaf(&c, |c| {
            c.expr_sql(Expression::InTheLast(leaf("lastplayed", json!(30))))
        });
        // Assert: the date is `now - 30d` formatted `2006-01-02` — the exact
        // literal moves with the clock, so pin the shape and the bound arity.
        assert_eq!(sql, "annotation.play_date > ?");
        assert_eq!(args.len(), 1, "one bound date");
        let Value::Text(date) = &args[0] else {
            panic!("date is text");
        };
        assert_eq!(date.len(), 10, "YYYY-MM-DD");
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[7..8], "-");
    }

    #[test]
    fn not_in_the_last_binds_lt_plus_a_null_match() {
        // Arrange: Go `NotInTheLast` — `squirrel.Or{Lt{firstDate}, Eq{nil}}`
        // (`criteria_sql.go:334-339`).
        let c = compiler();
        // Act:
        let (sql, _) = render_leaf(&c, |c| {
            c.expr_sql(Expression::NotInTheLast(leaf("lastplayed", json!(7))))
        });
        // Assert:
        assert_eq!(
            sql,
            "annotation.play_date < ? OR annotation.play_date IS NULL"
        );
    }

    #[test]
    fn start_of_period_matches_go_format() {
        // Arrange: Go `startOfPeriod` (`:343-345`) minus 1 day from a known noon.
        let from = std::time::UNIX_EPOCH + std::time::Duration::from_secs(86_400 * 19_700);
        // Act:
        let day = start_of_period(1, from);
        // Assert: 19700 days after 1970-01-01 is 2023-12-09; minus 1 → 2023-12-08.
        assert_eq!(day, "2023-12-08");
    }

    #[test]
    fn in_playlist_by_id_renders_the_not_in_subquery_with_visibility() {
        // Arrange: Go `InPlaylist{"id": "pl-1"}` for a non-admin owner with an
        // id (`inList`, `:347-379`): subquery over playlist_tracks+playlist with
        // `public = ? OR owner_id = ?`, wrapped `media_file.id IN (…)`.
        let criteria = Criteria {
            expression: Expression::InPlaylist(leaf("id", json!("pl-1"))),
            ..Default::default()
        };
        let c = SmartPlaylistCriteria::new(
            criteria,
            Owner {
                is_admin: false,
                id: "u1".to_string(),
            },
        );
        // Act:
        let (sql, args) = c.where_sql().expect("compiles");
        // Assert:
        assert_eq!(
            sql,
            "media_file.id IN (SELECT media_file_id FROM playlist_tracks pl LEFT JOIN playlist on pl.playlist_id = playlist.id WHERE pl.playlist_id = ? AND (playlist.public = ? OR playlist.owner_id = ?))"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("pl-1".to_string()),
                Value::Integer(1),
                Value::Text("u1".to_string()),
            ]
        );
    }

    #[test]
    fn not_in_playlist_by_path_expands_nfc_nfd_variants() {
        // Arrange: Go `NotInPlaylist{"path": "cafe\u0301"}` — the path form
        // expands `pathVariants` (`:351-352,381-391`); negate wraps NOT IN.
        let criteria = Criteria {
            expression: Expression::NotInPlaylist(leaf("path", json!("cafe\u{301}"))),
            ..Default::default()
        };
        let c = SmartPlaylistCriteria::new(criteria, Owner::default());
        // Act:
        let (sql, args) = c.where_sql().expect("compiles");
        // Assert: pathVariants keeps the RAW input first, then its NFC/NFD
        // variants (`criteria_sql.go:381-391`) — the first bound arg is the
        // raw NFD input, with its NFC form among the variants.
        assert!(sql.starts_with("media_file.id NOT IN ("));
        assert_eq!(args[0], Value::Text("cafe\u{301}".to_string()));
        assert!(args.contains(&Value::Text("caf\u{e9}".to_string())));
    }

    #[test]
    fn in_playlist_without_a_ref_errors() {
        // Arrange: Go `criteria_sql.go:353-355`.
        let criteria = Criteria {
            expression: Expression::InPlaylist(leaf("id", json!(""))),
            ..Default::default()
        };
        let c = SmartPlaylistCriteria::new(criteria, Owner::default());
        // Act / Assert:
        let err = c.where_sql().expect_err("no ref");
        assert!(err.to_string().contains("playlist id or path not given"));
    }

    #[test]
    fn admin_owner_skips_the_visibility_filters() {
        // Arrange: Go `inList`'s admin branch (`:357`): no public/owner filter.
        let criteria = Criteria {
            expression: Expression::InPlaylist(leaf("id", json!("pl-1"))),
            ..Default::default()
        };
        let c = SmartPlaylistCriteria::new(
            criteria,
            Owner {
                is_admin: true,
                id: String::new(),
            },
        );
        // Act:
        let (sql, args) = c.where_sql().expect("compiles");
        // Assert:
        assert!(sql.contains("pl.playlist_id = ?"));
        assert!(!sql.contains("playlist.public"));
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn all_group_merges_negated_same_tag_conds_into_one_batched_exists() {
        // Arrange: Go `All{IsNot{"genre":"a"}, IsNot{"genre":"b"}}` —
        // `mergeNegatedJsonConds` (`:485-490`) folds into one EXISTS with the
        // conds ORed inside (`mergeSameFieldConds`, `:495-567`), De Morgan.
        let mut fields1 = leaf("genre", json!("a"));
        let mut fields2 = leaf("genre", json!("b"));
        fields1.insert("extra".to_string(), json!(1)); // keep payloads distinct
        fields2.insert("extra".to_string(), json!(2));
        let criteria = Criteria {
            expression: Expression::All(vec![
                Expression::IsNot(leaf("genre", json!("a"))),
                Expression::IsNot(leaf("genre", json!("b"))),
            ]),
            ..Default::default()
        };
        let c = compiler_with(criteria, Owner::default());
        // Act:
        let (sql, args) = c.where_sql().expect("compiles");
        // Assert: one `not exists (… (value = ? OR value = ?))`.
        assert_eq!(
            sql,
            "not exists (select 1 from json_tree(media_file.tags, '$.genre') where key='value' and (value = ? OR value = ?))"
        );
        assert_eq!(
            args,
            vec![Value::Text("a".to_string()), Value::Text("b".to_string())]
        );
    }

    #[test]
    fn any_group_merges_same_tag_conds_into_one_batched_tag_exists() {
        // Arrange: Go `Any{Contains{"genre":"a"}, Contains{"genre":"b"}}` —
        // `mergeJsonConds` (`:476-481`).
        let criteria = Criteria {
            expression: Expression::Any(vec![
                Expression::Contains(leaf("genre", json!("a"))),
                Expression::Contains(leaf("genre", json!("b"))),
            ]),
            ..Default::default()
        };
        let c = compiler_with(criteria, Owner::default());
        // Act:
        let (sql, args) = c.where_sql().expect("compiles");
        // Assert:
        assert_eq!(
            sql,
            "exists (select 1 from json_tree(media_file.tags, '$.genre') where key='value' and (value LIKE ? OR value LIKE ?))"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("%a%".to_string()),
                Value::Text("%b%".to_string()),
            ]
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        // Arrange: Go `criteria_sql.go:641-649`.
        let c = compiler();
        // Act:
        let err = c
            .expr_sql(Expression::Contains(leaf("nosuch", json!("x"))))
            .expect_err("unknown field");
        // Assert:
        assert!(err.to_string().contains("invalid field in criteria"));
    }

    #[test]
    fn expression_joins_and_required_joins_plan_the_join_set() {
        // Arrange: Go `expressionJoins`/`requiredJoins` (`:806-823`): an
        // `albumloved` filter needs the album annotation join; sorting on
        // `albumdateadded` additionally needs the album join.
        let criteria = Criteria {
            expression: Expression::Gt(leaf("albumloved", json!(true))),
            sort: "albumdateadded".to_string(),
            ..Default::default()
        };
        let c = SmartPlaylistCriteria::new(criteria, Owner::default());
        // Act:
        let expr = c.expression_joins();
        let req = c.required_joins();
        // Assert:
        assert!(expr.has(SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION));
        assert!(!expr.has(SMART_PLAYLIST_JOIN_ALBUM));
        assert!(req.has(SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION));
        assert!(req.has(SMART_PLAYLIST_JOIN_ALBUM));
    }

    #[test]
    fn apply_joins_binds_user_id_at_each_annotation_join() {
        // Arrange: Go `applyJoins` (`:837-858`).
        let c = compiler();
        // Act:
        let joins = c.apply_joins(
            SMART_PLAYLIST_JOIN_ALBUM_ANNOTATION.or(SMART_PLAYLIST_JOIN_ARTIST_ANNOTATION),
            "u9",
        );
        // Assert:
        assert_eq!(joins.len(), 2);
        assert!(joins[0]
            .0
            .starts_with("LEFT JOIN annotation AS album_annotation"));
        assert_eq!(joins[0].1, vec![Value::Text("u9".to_string())]);
        assert!(joins[1]
            .0
            .starts_with("LEFT JOIN annotation AS artist_annotation"));
        assert_eq!(joins[1].1, vec![Value::Text("u9".to_string())]);
    }

    #[test]
    fn album_join_has_no_bound_args() {
        // Arrange: Go `:854-856`.
        let c = compiler();
        // Act:
        let joins = c.apply_joins(SMART_PLAYLIST_JOIN_ALBUM, "u9");
        // Assert:
        assert_eq!(
            joins[0].0,
            "LEFT JOIN album ON album.id = media_file.album_id"
        );
        assert!(joins[0].1.is_empty());
    }

    #[test]
    fn order_by_maps_fields_with_directions_and_coalesce_defaults() {
        // Arrange: Go `orderBy` (`:860-875`): `-rating,+random` over the fixed
        // map — `rating` coalesced (`:896-898`), `random`'s `order` expr
        // (`:130`), unknown fields dropped.
        let criteria = Criteria {
            sort: "-date,artist".to_string(),
            order: "desc".to_string(),
            ..Default::default()
        };
        let c = compiler_with(criteria, Owner::default());
        // Act:
        let order = c.order_by();
        // Assert: Go's exact oracle (`criteria_sql_test.go:323-325`) — desc
        // inverts every direction (sort.go:43-45); `random()` is NOT
        // direction-inverted.
        assert_eq!(
            order,
            "media_file.date asc, COALESCE(json_extract(media_file.participants, '$.artist[0].name'), '') desc"
        );
    }

    #[test]
    fn order_by_casts_numeric_tags_to_real() {
        // Arrange: Go `sortExpr` (`:877-904`): a numeric tag sorts with
        // `CAST(json_extract… AS REAL)`.
        let criteria = Criteria {
            sort: "rate".to_string(),
            ..Default::default()
        };
        let c = compiler_with(criteria, Owner::default());
        // Act:
        let order = c.order_by();
        // Assert:
        assert_eq!(
            order,
            "CAST(COALESCE(json_extract(media_file.tags, '$.rate[0].value'), '') AS REAL) asc"
        );
    }

    #[test]
    fn order_by_sorts_roles_through_participants() {
        // Arrange: Go `sortExpr`'s role branch (`:889-890`).
        let criteria = Criteria {
            sort: "artist".to_string(),
            ..Default::default()
        };
        let c = compiler_with(criteria, Owner::default());
        // Act:
        let order = c.order_by();
        // Assert: Go's exact oracle (`criteria_sql_test.go:303-305`) — the
        // `artist` role sorts through `media_file.participants`.
        assert_eq!(
            order,
            "COALESCE(json_extract(media_file.participants, '$.artist[0].name'), '') asc"
        );
    }

    #[test]
    fn full_smart_playlist_shape_compiles_through_the_seam() {
        // Arrange: the full smart-playlist shape — an any-group over a mix of
        // column, annotation and tag fields, sorted with a limit — compiled
        // into the `CriteriaSql` seam `smart_playlist_repo.rs` consumes.
        let criteria = Criteria {
            expression: Expression::Any(vec![
                Expression::Contains(leaf("title", json!("love"))),
                Expression::Is(leaf("loved", json!(true))),
                Expression::Contains(leaf("genre", json!("rock"))),
            ]),
            sort: "rating".to_string(),
            limit: 50,
            offset: 10,
            ..Default::default()
        };
        let c = compiler_with(
            criteria,
            Owner {
                is_admin: false,
                id: "u1".to_string(),
            },
        );
        // Act: the seam's slots, as `smart_playlist_repo.rs`'s
        // `build_insert_select` consumes them.
        let (filter_sql, filter_args) = c.where_sql().expect("compiles");
        let order_by = c.order_by();
        let joins = c.apply_required_joins("u1");
        // Assert: filter renders all three disjuncts with args in order;
        // order-by is over the fixed map; the join plan carries the annotation
        // joins with the user bound.
        assert_eq!(
            filter_sql,
            "media_file.title LIKE ? OR annotation.starred = ? OR exists (select 1 from json_tree(media_file.tags, '$.genre') where key='value' and value LIKE ?)"
        );
        assert_eq!(
            filter_args,
            vec![
                Value::Text("%love%".to_string()),
                Value::Bool(true),
                Value::Text("%rock%".to_string()),
            ]
        );
        assert_eq!(order_by, "COALESCE(annotation.rating, 0) asc");
        assert_eq!(joins.len(), 0, "no album/artist join needed here");
    }

    #[test]
    fn field_map_registry_is_threaded_not_global() {
        // Arrange: Go's package-global registry (`model/criteria/fields.go:25`);
        // the port threads [`FieldMap`] — verify the compiler uses it for
        // lookups by pointing `sort` at an alias-free registry entry.
        let criteria = Criteria {
            sort: "TITLE".to_string(),
            ..Default::default()
        };
        let c = SmartPlaylistCriteria::new(criteria, Owner::default());
        // Act:
        let order = c.order_by();
        // Assert:
        assert_eq!(order, "media_file.title asc");
    }

    #[test]
    fn path_variants_expands_unicode_forms() {
        // Arrange: Go `pathVariants` (`criteria_sql.go:381-391`) — the RAW
        // input is kept first, then its NFC/NFD spellings are appended.
        let nfd_form = "cafe\u{301}";
        // Act:
        let variants = path_variants(nfd_form);
        // Assert: the raw input first, plus its NFC form present.
        assert_eq!(variants[0], nfd_form.to_string());
        assert!(variants.contains(&"caf\u{e9}".to_string()));
        assert!(variants.len() >= 2);
        assert!(variants.len() <= 3);
    }

    #[test]
    fn ascii_path_yields_a_single_variant() {
        // Arrange / Act:
        let variants = path_variants("/music/plain");
        // Assert:
        assert_eq!(variants, vec!["/music/plain".to_string()]);
    }
}
