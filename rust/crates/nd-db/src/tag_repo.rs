//! Port of `persistence/tag_repository.go` (107L) — the tag dictionary
//! repository, built on the `baseTagRepository` select shape of
//! `persistence/sql_tags.go:148-231`.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `executeSQL`/`queryAll`, which materialize through `toSQL()`
//! (`sql_base_repository.go:358-372`) as `(sql, args)`; execution, `WithTx`
//! and logging stay with `store.rs`/the caller, so this port stops at the same
//! `(sql, args)` pairs (see [`Sql`]).
//!
//! Ported and EXECUTED here: `Add`'s chunked `tag` + `library_tag` inserts
//! (`:26-50`, `slices.Chunk(tags, 200)` → [`CHUNK_SIZE`]) run on the caller's
//! transaction through [`add_in_tx`], with [`add_sql`] left as the renderer it
//! executes; `UpdateCounts`'s two templated upserts (`:48-69`) stay RENDERED
//! only (DB-17 owns their execution), `GetAll`'s name-filtered aggregate select
//! (`baseTagRepository.newSelect`, `sql_tags.go:180-198`) is EXECUTED through
//! [`TagRepo::get_all`], and the private `purgeUnused` delete (`:85-103`) stays
//! rendered (DB-20 owns it).
//!
//! Not ported, with the reason: the base repository's REST surface
//! (`Count`/`Read`/`ReadAll`, `sql_tags.go:202-231`) is NOT exposed here as its own
//! methods — `Read`'s unqualified `Where(Eq{"id": id})` (`:217`) and `Count`'s
//! `Select("COUNT(DISTINCT tag.id)")` shape (`:203-210`) are recorded below with
//! their executed traces so a later lane can port them, and this slice's scope is
//! `GetAll`. `ReadAll` (`:223-228`) needs no separate method: it is
//! `newSelect(ctx, parseRestOptions(…))`, i.e. exactly [`get_all_sql`] fed
//! [`parse_rest_options`] — which IS ported and executed, and is now called INSIDE
//! [`TagRepo::get_all`] just as Go calls `parseRestOptions` inside `ReadAll`
//! (`sql_tags.go:224`), so the executed read cannot be handed an unsanitized sort.
//!
//! Clause order is a real parity question on this select, and the executed Go
//! traces settle it. `sqlRepository.newSelect` calls `applyFilters` right after
//! `applyOptions` (`sql_base_repository.go:140-146`), and squirrel ANDs repeated
//! `.Where()` calls in CALL order — so the caller's filters come BEFORE the tag
//! name clause that `baseTagRepository.newSelect` (`sql_tags.go:183-186`) and
//! `tagRepository.GetAll` (`tag_repository.go:79`) add later. Executed (oracle
//! `/tmp/sd3g1_oracle6`, `/tmp/sd3g1_oracle8`):
//! * `GetAll("genre", Filters: Eq{"library_tag.library_id": 2})` →
//!   `WHERE library_tag.library_id = ? AND tag.tag_name = ?`, args
//!   `[adm, 2, genre]` (user id first: squirrel binds JOIN args before WHERE args);
//! * the same filter as `And{Eq{library_tag.library_id:1}, Eq{tag.tag_value:Rock}}`
//!   → `WHERE (library_tag.library_id = ? AND tag.tag_value = ?) AND tag.tag_name = ?`
//!   — the `And` renders parenthesized (squirrel `conj.join`,
//!   `squirrel@v1.5.4/expr.go:369-388`), so the port's AND-joined
//!   [`crate::sql_restful::Filter`] list needs no extra parentheses; an EMPTY `And`
//!   renders `(1=1)` (`expr.go:14`, `:393-395`), which is Go's admin/user-filter
//!   identity and matches what [`crate::sql_restful`] already renders for it;
//! * `ReadAll(Filters:{"name":"oc"})` → `WHERE (tag_value LIKE ?)` with `%oc%` —
//!   the registered `name` filter is `containsFilter("tag_value")`
//!   (`sql_tags.go:155-158`), NOT a `tag_name` clause.
//!
//! Sort: this repository HAS a sort mapping — `setSortMappings({"name":
//! "tag_value"})` (`sql_tags.go:173-175`) — so [`get_all_sql`] renders a real
//! ORDER BY leg through [`build_sort_order`]. Executed: `GetAll("genre",
//! Sort:"name", Order:"desc")` traces `… GROUP BY tag.id, tag.tag_name,
//! tag.tag_value ORDER BY tag_value desc` and returns `Rock/2/5, Pop/0/0`; the
//! un-mapped `Sort:"album_count"` traces `ORDER BY album_count desc LIMIT 2`.
//! That leg sits AFTER GROUP BY, which is squirrel's clause kind order
//! (`sqlrepo.rs:352-356`) and Go's trace order.

use crate::runtime_error::DbOperationError;
use crate::sql_restful::RestRepository;
use crate::sql_tags::{library_filter_joins, Sql};
use crate::sqlrepo::{Repository, Value};

/// `slices.Chunk(tags, 200)` (`tag_repository.go:27`): rows per multi-row
/// INSERT statement pair.
pub const CHUNK_SIZE: usize = 200;

/// The `tag` dictionary table (`newBaseTagRepository`, `sql_tags.go:154`).
pub const TAG_TABLE: &str = "tag";

/// The `library_tag` per-library statistics table (`tag_repository.go:39`).
pub const LIBRARY_TAG_TABLE: &str = "library_tag";

/// `model.Tag` (`model/tag.go`): the dictionary row plus the aggregate counts
/// `GetAll`'s COALESCE columns fill (`sql_tags.go:192-193`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tag {
    /// Dictionary id — `model.NewTag(name, value).ID`.
    pub id: String,
    /// Tag name, e.g. `genre`.
    pub tag_name: String,
    /// Display value.
    pub tag_value: String,
    /// `COALESCE(SUM(library_tag.album_count), 0)` across the user's libraries.
    pub album_count: i64,
    /// `COALESCE(SUM(library_tag.media_file_count), 0)`, Go's `SongCount`.
    pub song_count: i64,
}

/// `model.TagList`.
pub type TagList = Vec<Tag>;

/// One `Add` chunk's two statements, in Go's execution order
/// (`tag_repository.go:26-44`): the dictionary insert runs first, then the
/// `library_tag` rows; Go fails the whole `Add` if the second errors.
#[derive(Debug, Clone, PartialEq)]
pub struct AddChunkSql {
    /// `Insert(tag).Columns(id, tag_name, tag_value).Suffix("on conflict (id)
    /// do nothing")` — one VALUES row per tag of the chunk.
    pub tag_insert: Sql,
    /// `Insert(library_tag).Columns(tag_id, library_id, album_count,
    /// media_file_count).Suffix("on conflict (tag_id, library_id) do nothing")`
    /// — zeroed counts; `UpdateCounts` fills them.
    pub library_tag_insert: Sql,
}

/// `Add` (`tag_repository.go:26-50`): the chunked insert pairs, RENDERED. Go
/// builds one pair per `slices.Chunk(tags, 200)` and executes dictionary-then-link
/// (`:28-47`); [`add_in_tx`] executes exactly these pairs, so nothing here is a
/// second renderer. Idempotent via both `ON CONFLICT … DO NOTHING` suffixes, so a
/// re-scan re-adds safely, and an empty `tags` renders — and executes — nothing.
/// Counts are NOT part of the contract: the link rows carry literal `0, 0` and
/// `ON CONFLICT DO NOTHING` leaves an existing row's counts untouched.
#[must_use]
pub fn add_sql(library_id: i64, tags: &[Tag]) -> Vec<AddChunkSql> {
    tags.chunks(CHUNK_SIZE)
        .map(|chunk| {
            let mut tag_rows = String::new();
            let mut tag_args = Vec::new();
            let mut lib_rows = String::new();
            let mut lib_args = Vec::new();
            for (i, tag) in chunk.iter().enumerate() {
                if i > 0 {
                    tag_rows.push_str(", ");
                    lib_rows.push_str(", ");
                }
                tag_rows.push_str("(?, ?, ?)");
                tag_args.push(Value::from(tag.id.clone()));
                tag_args.push(Value::from(tag.tag_name.clone()));
                tag_args.push(Value::from(tag.tag_value.clone()));
                lib_rows.push_str("(?, ?, 0, 0)");
                lib_args.push(Value::from(tag.id.clone()));
                lib_args.push(Value::Integer(library_id));
            }
            AddChunkSql {
                tag_insert: (
                    format!(
                        "INSERT INTO {TAG_TABLE} (id, tag_name, tag_value) VALUES {tag_rows} on conflict (id) do nothing"
                    ),
                    tag_args,
                ),
                library_tag_insert: (
                    format!(
                        "INSERT INTO {LIBRARY_TAG_TABLE} (tag_id, library_id, album_count, media_file_count) VALUES {lib_rows} on conflict (tag_id, library_id) do nothing"
                    ),
                    lib_args,
                ),
            }
        })
        .collect()
}

/// Callback error of the executed tag [`add_in_tx`], which has NO domain failure
/// to carry: Go's `Add` returns a bare `error` that can only be a driver failure
/// (`tag_repository.go:33-36`, `:44-47`) — no validation, no `NotFound`, no
/// permission check. `Infallible` makes the `Domain` arm structurally uninhabited
/// rather than reserved, as in `library_repo::LibraryScanError`, and keeps every
/// SQLite failure on the [`DbOperationError::Sqlite`] arm so `store::is_busy`
/// still classifies `SQLITE_BUSY` for the executor's retry loop.
pub type TagWriteError = DbOperationError<std::convert::Infallible>;

/// `Add` (`tag_repository.go:26-50`) EXECUTED on the CALLER's transaction: per
/// 200-row chunk the dictionary insert runs first, then the `library_tag` link
/// (`:28-47`), both from [`add_sql`] rather than re-rendered here. An empty `tags`
/// yields no chunk, so the call issues NO statement at all — not an empty insert.
///
/// What this deliberately does NOT do, all of it Go parity:
/// * **Ids are taken verbatim.** `Add` binds `t.ID` (`:31`) and never derives or
///   validates it; `model.NewTag`/`nd_id::new_tag_id` belong to the caller that
///   builds tags upstream (`model/tag.go:63-71`, `Tags.FlattenAll`). A caller
///   handing back an id that disagrees with its name/value is stored as-is, and
///   the call neither recomputes, dedupes, nor rejects.
/// * **No validation of name or value.** Both DDL columns are `default '' not
///   null` (`nd-dbschema/src/schema_ddl.rs:55`), so an empty or adversarial value
///   is a storable value; Go validates nothing in `Add`.
/// * **No dedupe at this boundary.** Duplicate input rows are legal for `Add` — the
///   scanner dedupes by tag ID upstream in `loadTagsFromFiles`
///   (`scanner/phase_1_folders.go:294,305-306,323`), but `Add` itself performs no
///   dedupe and accepts repeats, and
///   `on conflict (id) do nothing` collapses them to one dictionary row and one
///   link row (EXECUTED-RUST in `tests/runtime_tag_writes.rs`: repeated input
///   collapses to one dictionary row and one link row).
/// * **No counts reset.** Link rows insert literal `0, 0` and the composite
///   `on conflict (tag_id, library_id) do nothing` leaves an existing row's
///   counts intact, so re-`Add` cannot zero them; `UpdateCounts` (DB-17) owns
///   that column and until it runs a freshly-added tag reports `0/0`.
///
/// One library per call: `Add` takes a scalar `libraryID` (`:26`), so every tag
/// here links to that library and a caller needing two libraries calls it twice.
///
/// Both legs share the caller's transaction, so a failure anywhere rolls the
/// whole bundle back. That is load-bearing rather than cosmetic: EXECUTED-RUST in
/// `tests/runtime_tag_writes.rs`, the same two rendered chunks run in AUTOCOMMIT on
/// an FK-on connection — what Go's `Add` does when no transaction wraps it — fail
/// with `SQLITE_CONSTRAINT_FOREIGNKEY` and LEAVE the dictionary rows committed. That
/// leak is Go's own behavior untransacted, and the tx-bound API is what prevents it.
///
/// # Errors
/// [`DbOperationError::Sqlite`] carrying the concrete `rusqlite::Error` (extended
/// code intact) from either leg: the `SQLITE_CONSTRAINT_FOREIGNKEY` of a missing
/// library, and the table-level `tags_name_value` UNIQUE (`schema_ddl.rs:55`,
/// `unique (tag_name, tag_value)`) when two DIFFERENT ids carry the same
/// `(tag_name, tag_value)` — that constraint is not covered by the `(id)`
/// conflict target, so it is a hard failure of the whole `Add` in Go too
/// (`:28-29` uses the same target). Never [`DbOperationError::Domain`]:
/// [`TagWriteError`] has no inhabited domain arm.
///
/// Divergence, deliberate: Go wraps the SECOND leg as
/// `fmt.Errorf("adding library_tag entries: %w", err)` (`:46`), so Go's error TEXT
/// gains a prefix. The port keeps the raw `rusqlite::Error` on the `Sqlite` arm
/// instead, because the executor's retry classifier matches on that type — a
/// stringified wrapper would hide `SQLITE_BUSY` (`decisions.md` E1). Same call as
/// `library_repo::add_artist_in_tx` records.
pub fn add_in_tx(
    tx: &rusqlite::Transaction<'_>,
    library_id: i64,
    tags: &[Tag],
) -> Result<(), TagWriteError> {
    for chunk in add_sql(library_id, tags) {
        crate::execution::execute(tx, &chunk.tag_insert).map_err(DbOperationError::sqlite)?;
        crate::execution::execute(tx, &chunk.library_tag_insert)
            .map_err(DbOperationError::sqlite)?;
    }
    Ok(())
}

/// `UpdateCounts` (`tag_repository.go:48-69`): one upsert per stats table
/// (`album`, `media_file`), Go's `fmt.Sprintf(template, table)` with `%[1]s`
/// = the table name. Only genres (`json_tree(tags, '$.genre')`) aggregate.
#[must_use]
pub fn update_counts_sql() -> Vec<Sql> {
    ["album", "media_file"]
        .iter()
        .map(|table| {
            (
                format!(
                    "\
INSERT INTO library_tag (tag_id, library_id, {table}_count)
SELECT jt.value as tag_id, {table}.library_id, count(distinct {table}.id) as {table}_count
FROM {table}
JOIN json_tree({table}.tags, '$.genre') as jt ON jt.atom IS NOT NULL AND jt.key = 'id'
JOIN tag ON tag.id = jt.value
GROUP BY jt.value, {table}.library_id
ON CONFLICT (tag_id, library_id)
DO UPDATE SET {table}_count = excluded.{table}_count;"
                ),
                Vec::new(),
            )
        })
        .collect()
}

/// The private `purgeUnused` (`tag_repository.go:86-103`): delete dictionary
/// rows no `album`/`media_file` `tags` JSON references — the two `json_tree`
/// scans UNION'd, exactly Go's fragment (`:88-98`).
#[must_use]
pub fn purge_unused_sql() -> Sql {
    (
        format!(
            "DELETE FROM {TAG_TABLE} WHERE \
             id not in (select jt.value \
             from album left join json_tree(album.tags, '$') as jt \
             where atom is not null \
             and key = 'id' \
             UNION \
             select jt.value \
             from media_file left join json_tree(media_file.tags, '$') as jt \
             where atom is not null \
             and key = 'id')"
        ),
        Vec::new(),
    )
}

/// `setSortMappings(map[string]string{"name": "tag_value"})`
/// (`sql_tags.go:159-161`, set by `newBaseTagRepository`, so it applies to the tag
/// dictionary AND to the genre repository). Kept as a constant so the one-entry map
/// is a pinned fact rather than an accident.
pub const SORT_MAPPINGS: [(&str, &str); 1] = [("name", "tag_value")];

/// `lookupSortMapping` (`sql_base_repository.go:187-197`): direct hit, then the
/// snake_cased hit. Go also probes `toCamelCase(sort)` between the two; with this
/// one-key map that arm can only fire on the key itself, which the direct arm
/// already catches, so it is not reproduced.
#[must_use]
pub fn sort_mapping(sort: &str) -> Option<&'static str> {
    SORT_MAPPINGS
        .iter()
        .find(|(key, _)| *key == sort || *key == crate::sql_restful::to_snake_case(sort))
        .map(|(_, value)| *value)
}

/// `sortMapping` (`sql_base_repository.go:165-183`): resolve each comma-separated
/// part through [`sort_mapping`], snake-casing the parts it does not know.
/// Executed (oracle `/tmp/sd3g1_oracle6`): `Sort:"name"` resolves to the `tag_value`
/// mapping (`ORDER BY tag_value desc`), and the un-mapped `Sort:"album_count"` keeps
/// its snake-cased alias (`ORDER BY album_count desc LIMIT 2`).
#[must_use]
pub fn mapped_sort(sort: &str) -> String {
    crate::sql_restful::split_sort_fields(sort, ',')
        .iter()
        .map(|part| {
            let part = part.trim();
            match sort_mapping(part) {
                Some(mapping) => mapping.to_string(),
                None => crate::sql_restful::to_snake_case(part),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `buildSortOrder` (`sql_base_repository.go:199-228`) over THIS repository's
/// mappings. Local rather than [`crate::player_repo::build_sort_order`], which
/// resolves the player's map — Go's splitter is per-repository for exactly that
/// reason (the same decision is recorded in `library_repo::build_sort_order`).
#[must_use]
pub fn build_sort_order(sort: &str, order: &str) -> String {
    let order = order.trim().to_lowercase();
    let (order, reverse) = if order == "desc" {
        ("desc".to_string(), "asc".to_string())
    } else {
        ("asc".to_string(), "desc".to_string())
    };
    crate::sql_restful::split_sort_fields(&mapped_sort(sort), ',')
        .iter()
        .map(|part| {
            let fields = crate::sql_restful::split_sort_fields(part, ' ');
            let field = fields.first().map(String::as_str).unwrap_or("");
            let direction = match fields.get(1).map(String::as_str) {
                None | Some("asc") => order.clone(),
                Some(_) => reverse.clone(),
            };
            format!("{field} {direction}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `tagLibraryIdFilter` (`sql_tags.go:135-137`) as a `FilterFn`: the clause the
/// repository registers for its `library_id` key (`sql_tags.go:157`).
/// [`crate::sql_tags::where_tag_library_id`] is the same clause for a builder
/// caller; the rest layer needs it as a filter function, so it is built here.
pub fn tag_library_id_filter() -> crate::sql_restful::FilterFn {
    Box::new(|_field: &str, value: &str| {
        Some(crate::sql_restful::Filter::Eq {
            column: format!("{LIBRARY_TAG_TABLE}.library_id"),
            value: Value::from(value),
        })
    })
}

/// `registerModel(&model.Tag{}, map[string]filterFunc{…})` plus
/// `registerModelWhiteList` (`sql_tags.go:155-158`,
/// `sql_base_repository.go:104-111`, `sql_restful.go:142-163`): the two filter
/// mappings Go registers for the tag dictionary, and the whitelist of
/// `model.Tag`'s field names plus the annotation fields.
///
/// The mappings are the executed difference between the two filter spellings:
/// `Filters:{"name":"oc"}` traces `WHERE (tag_value LIKE ?)` bound to `%oc%`
/// (`containsFilter("tag_value")`, `sql_tags.go:156`, `sql_restful.go:98-102`) and
/// `Filters:{"library_id":"2"}` traces `WHERE (library_tag.library_id = ?)` bound to
/// `"2"` (`sql_tags.go:157`). Without the mappings both would fall through to the
/// whitelist arm and render `name LIKE ?` / `library_id = ?` against columns this
/// projection does not emit. Executed (oracle `/tmp/sd3g1_oracle8`): the first
/// returns `Rock/2/5`, the second returns nothing for a user with access only to
/// library 1.
#[must_use]
pub fn rest_repo() -> RestRepository {
    RestRepository::new(TAG_TABLE)
        .with_filter_mappings([
            ("name", crate::sql_restful::contains_filter("tag_value")),
            ("library_id", tag_library_id_filter()),
        ])
        .with_field_white_list(["ID", "TagName", "TagValue", "AlbumCount", "SongCount"])
}

/// `sanitizeSort` for this repository (`sql_restful.go:64-82`, which validates
/// against `lookupSortMapping` OR the field whitelist). The mapping arm is what
/// keeps `Sort:"name"` — the whitelist has `tag_name`, not `name` — so the probe is
/// not redundant here. Executed: `ReadAll(Sort:"bogus")` traces NO ORDER BY beside
/// the `Ignoring sort not whitelisted` warning, while `ReadAll(Sort:"name",
/// Order:"desc")` traces `ORDER BY tag_value desc` (oracle `/tmp/sd3g1_oracle8`).
#[must_use]
pub fn sanitize_sort(sort: &str, order: &str) -> (String, String) {
    let (whitelisted, order) = rest_repo().sanitize_sort(sort, order);
    if sort.is_empty() {
        return (String::new(), order);
    }
    let snake = crate::sql_restful::to_snake_case(sort);
    let kept = if sort_mapping(&snake).is_some() || !whitelisted.is_empty() {
        snake
    } else {
        String::new()
    };
    (kept, order)
}

/// `parseRestOptions` for this repository (`sql_restful.go:54-67`): the
/// `QueryOptions` [`get_all_sql`] consumes. Go's `baseTagRepository.ReadAll`
/// (`sql_tags.go:219-222`) is exactly `newSelect(ctx, parseRestOptions(…))`, so
/// [`TagRepo::get_all`] under a [`parse_rest_options`] result IS the REST read — no
/// second statement builder.
#[must_use]
pub fn parse_rest_options(
    options: Option<&crate::sql_restful::RestQueryOptions>,
) -> crate::sql_restful::QueryOptions {
    let Some(options) = options else {
        return crate::sql_restful::QueryOptions::default();
    };
    let (sort, order) = sanitize_sort(&options.sort, &options.order);
    crate::sql_restful::QueryOptions {
        sort,
        order,
        max: options.max,
        offset: options.offset,
        seed: options.filters.get("seed").cloned(),
        filters: rest_repo().parse_rest_filters(options),
    }
}

/// Render [`crate::sql_tags::library_filter_joins`]'s fragments with Go's JOIN
/// KINDS (`baseTagRepository.applyLibraryFiltering`, `sql_tags.go:167-175`):
/// `library_tag` is `sq.LeftJoin` (`:168`) and `user_library` is a bare `sq.Join`
/// (`:173`), which squirrel renders as plain `JOIN` = INNER
/// (`squirrel@v1.5.4/select.go:297-299`).
///
/// Executed divergence, and the reason this fn exists (`/tmp/sd3g_leftjoin`, and
/// oracle `/tmp/sd3g1_oracle6` with libraries 1+2, admin `adm` joined to library 1
/// only, dictionary `Rock`/`Pop` in lib 1, `OnlyLib2` in lib 2, and `Lonely` with no
/// `library_tag` row at all):
/// * Go `GetAll("genre")` for `adm` returned **`Pop/0/0, Rock/2/5`** (len 2):
///   `OnlyLib2` and `Lonely` are BOTH absent, because INNER drops any aggregate row
///   whose only `library_tag` row is in an inaccessible library, and any dictionary
///   row with no `library_tag` row at all.
/// * The headless read, which never adds that join, returned all four rows.
/// * The parent's independent SQLite measurement of the same rows: plain `JOIN`
///   count = 1, `LEFT JOIN` count = 2 — i.e. rendering INNER as LEFT over-counts
///   rows the user cannot access.
///
/// So the previous `LEFT JOIN` rendering on the `user_library` leg (pre-existing at
/// HEAD, `tag_repo.rs` unit test asserted it) is provably wrong on this read and is
/// corrected here. `library_filter_joins` itself is untouched — `genre_repo.rs:116`
/// asserts the old shape and that lane is outside this slice's ownership, so the
/// same divergence remains open there (recorded in
/// `docs/rust-port/hydrate-contract.md` §S-D3g).
#[must_use]
fn join_clause(join: &str) -> String {
    if join.starts_with("user_library") {
        format!(" JOIN {join}")
    } else {
        format!(" LEFT JOIN {join}")
    }
}

/// `GetAll` (`tag_repository.go:78-83`) over the `baseTagRepository.newSelect`
/// shape (`sql_tags.go:180-198`): the name-filtered aggregate select.
/// `COALESCE(SUM(…, 0)` keeps rows with no `library_tag` match at zero counts, the
/// LEFT `library_tag` leg is why, and the authenticated user's INNER `user_library`
/// join scopes the SUM to accessible libraries (`None` = no logged-in user, and then
/// that join is not added at all — `sql_tags.go:170-175`).
///
/// `id` is the `baseTagRepository.Read` arm (`sql_tags.go:216-221`); `None` is
/// `GetAll`. See [`read_sql`] for why the tag-name clause is NOT added on that arm.
///
/// Clause order follows squirrel's clause KIND order, matching Go's executed
/// traces: WHERE (caller filters first, then the tag-name clause) → GROUP BY →
/// ORDER BY → LIMIT → OFFSET. Filters come first because `sqlRepository.newSelect`
/// runs `applyOptions`/`applyFilters` (`sql_base_repository.go:140-146`) before
/// `GetAll` appends `Where(Eq{"tag.tag_name": name})` (`tag_repository.go:79`), and
/// squirrel ANDs repeated `.Where()` calls in call order. Executed args bind in SQL
/// position: the `user_library` user id first (squirrel binds JOIN args before WHERE
/// args), then the filter values, then the tag name — `[adm, 2, genre]` for
/// `Filters: Eq{"library_tag.library_id": 2}` (oracle `/tmp/sd3g1_oracle6`).
///
/// WARNING: `options` here must ALREADY be the output of [`parse_rest_options`]
/// (`sql_restful.go:54-67`) — this renderer is a pure statement builder and does NOT
/// sanitize `options.sort` itself, so a hand-built
/// [`crate::sql_restful::QueryOptions`] carrying an arbitrary sort string reaches
/// `ORDER BY` verbatim through [`build_sort_order`] (injection through the sort field,
/// exactly as in `transcoding_repo.rs:161-167`). Go has no such gap because `ReadAll`
/// calls `parseRestOptions` itself (`sql_tags.go:224`) and `sanitizeSort`
/// (`sql_restful.go:69-86`) therefore always runs first; [`TagRepo::get_all`] mirrors
/// that by parsing its own rest options, which is why the executed path is safe. The
/// residual risk is confined to callers of this public renderer: the whitelist check
/// lives in [`sanitize_sort`], not here. Go's own renderer accepts whatever
/// `applyOptions` is handed (`sql_base_repository.go:157-158`), so this renderer is
/// deliberately NOT tightened.
///
/// A [`crate::sql_restful::Filter`] list renders unparenthesized and AND-joined
/// here, whereas Go's `And{}` renders `(a AND b)` (`squirrel@v1.5.4/expr.go:369-388`)
/// and an EMPTY `And` renders `(1=1)`. Both are row-set identical against this
/// select — executed: the `And{Eq{library_tag.library_id:1}, Eq{tag.tag_value:Rock}}`
/// arm and the port's AND-joined pair both returned exactly `Rock/2/5` — so no
/// parentheses are added (brief rule 4).
///
/// # Errors
/// [`crate::sqlrepo::SqlError::NoTable`] if the FROM fragment is missing (fixed
/// table ⇒ unreachable in practice), or `EmptyOrder` if a caller sets an explicitly
/// empty order-by list.
pub fn get_all_sql(
    tag_name: &str,
    user_id: Option<&str>,
    options: Option<&crate::sql_restful::QueryOptions>,
    id: Option<&str>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let (joins, join_args) = library_filter_joins(user_id.map(std::string::ToString::to_string));
    let join_sql = joins
        .iter()
        .map(|join| join_clause(join))
        .collect::<String>();
    let mut sq = Repository::new(TAG_TABLE).select([
        "tag.id",
        "tag.tag_name",
        "tag.tag_value",
        "COALESCE(SUM(library_tag.album_count), 0) as album_count",
        "COALESCE(SUM(library_tag.media_file_count), 0) as song_count",
    ]);
    if let Some(options) = options {
        sq = crate::sql_search::apply_pagination(sq, options);
        sq = crate::sql_search::apply_filters(sq, options);
        if !options.sort.is_empty() {
            sq = sq.order_by([build_sort_order(&options.sort, &options.order)]);
        }
    }
    if let Some(id) = id {
        // `Read` (`sql_tags.go:217`): `Where(Eq{"id": id})`, UNQUALIFIED, and this
        // is the tag dictionary instance whose `tagFilter` is nil
        // (`tag_repository.go:22`), so the executed trace has no tag-name clause:
        // `WHERE id = ?`.
        sq = sq.where_eq("id", id);
    } else {
        sq = sq.where_eq("tag.tag_name", tag_name);
    }
    sq = sq.group_by(["tag.id", "tag.tag_name", "tag.tag_value"]);
    let (mut sql, mut args) = sq.to_sql()?;
    // Splice the join fragments after FROM tag, before WHERE — squirrel's join
    // position; the join identifiers are fixed schema names (`sql_tags.go:168-174`)
    // and the only bound value is the user id, which binds at the JOIN position and
    // therefore ahead of every WHERE value.
    let from = format!("FROM {TAG_TABLE}");
    let Some(pos) = sql.find(&from) else {
        return Err(crate::sqlrepo::SqlError::NoTable);
    };
    let at = pos + from.len();
    sql.replace_range(at..at, &join_sql);
    args.splice(..0, join_args.iter().cloned());
    Ok((sql, args))
}

/// `GetAll` (`tag_repository.go:78-83`) with an empty option list: `newSelect(ctx)`
/// applies no filter, no page and no ORDER BY at all
/// (`sql_base_repository.go:142-145`), so rows come back in SQLite's order.
/// Executed trace: the aggregate select with `WHERE tag.tag_name = ?` plus the
/// GROUP BY, nothing after it.
pub fn get_all_plain_sql(
    tag_name: &str,
    user_id: Option<&str>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    get_all_sql(tag_name, user_id, None, None)
}

/// `baseTagRepository.Read` (`sql_tags.go:216-221`): the aggregate select with
/// `Where(Eq{"id": id})` and NO options — so no filters, no page, no sort leg, and
/// (for this dictionary instance, `tagFilter == nil`) no tag-name clause.
/// Executed (oracle `/tmp/sd3g1_oracle8`): `WHERE id = ? GROUP BY …`, args
/// `[adm, <tag id>]`; the accessible tag came back with its counts, and a tag whose
/// rows live only in an inaccessible library returned `data not found` — the INNER
/// join removes the row rather than yielding a zero-count tag.
pub fn read_sql(user_id: Option<&str>, id: &str) -> Result<Sql, crate::sqlrepo::SqlError> {
    get_all_sql("", user_id, None, Some(id))
}

/// `baseTagRepository.Count` (`sql_tags.go:202-214`) through `sqlRepository.count`
/// (`sql_base_repository.go:565-576`): the DISTINCT aggregate over the SAME join
/// legs plus `count`'s reset `ORDER BY tag.id`. No GROUP BY — the aggregate is the
/// whole select.
///
/// Differences from [`get_all_sql`], all from Go's own code and all executed:
/// * the tag dictionary's `tagFilter` is nil (`tag_repository.go:22`), so `Count`
///   adds NO tag-name clause — executed trace has none (`GetAll` does have it);
/// * `count` drops the columns/bounds and APPENDS `ORDER BY tag.id`
///   (`sql_base_repository.go:566-569`), and `Count` never calls `applyOptions`, so
///   caller `Sort`/`Max`/`Offset` cannot reach this statement — executed
///   `Count(Sort:"name", Order:"desc")` traces `… ORDER BY tag.id`, reset leg only;
/// * `count` applies filters AFTER the clauses already on the builder
///   (`sql_base_repository.go:571`), the reverse of `newSelect`'s order.
///
/// Executed values (oracle `/tmp/sd3g1_oracle8`; dictionary Rock/Pop/Chill in lib 1,
/// OnlyLib2 in lib 2, `Lonely` with no `library_tag` row): `adm` joined to lib 1
/// only → `3`; headless → `5`; `adm` with `Filters:{"name":"oc"}` → `1`; `adm` with
/// `Filters:{"library_id":"2"}` → `0`. Note the INNER-vs-LEFT hazard in
/// `join_clause`: a LEFT rendering would report `5` for the admin instead of `3`.
///
/// WARNING: `options` here must ALREADY be the output of [`parse_rest_options`]
/// (`sql_restful.go:54-67`), the same contract as [`get_all_sql`]. The parsed filters
/// are the only values this renderer accepts, and `count` resets every order leg to
/// `tag.id` (`sql_base_repository.go:566-569`), so neither a raw filter clause nor
/// `options.sort` can reach the statement text from a sanitized [`crate::sql_restful::QueryOptions`];
/// [`TagRepo::count_all`] keeps that contract. Go's REST `Count` parses rest options
/// itself (`sql_tags.go:213`), mirrored by [`TagRepo::count`]. Go's renderer
/// likewise accepts whatever `applyFilters` is handed (`sql_base_repository.go:571`),
/// so this renderer is deliberately NOT tightened.
///
/// # Errors
/// [`crate::sqlrepo::SqlError`] if the builder fails to materialize.
pub fn count_all_sql(
    user_id: Option<&str>,
    options: Option<&crate::sql_restful::QueryOptions>,
) -> Result<Sql, crate::sqlrepo::SqlError> {
    let (joins, join_args) = library_filter_joins(user_id.map(std::string::ToString::to_string));
    let join_sql = joins
        .iter()
        .map(|join| join_clause(join))
        .collect::<String>();
    let mut sq = Repository::new(TAG_TABLE);
    if let Some(options) = options {
        sq = crate::sql_search::apply_filters(sq, options);
    }
    // `count` replaces the columns and drops bounds/order
    // (`sql_base_repository.go:565-568`); `Repository::count` reproduces that, and
    // the reset leg is re-installed after it (:569).
    sq = sq.count(TAG_TABLE).order_by([format!("{TAG_TABLE}.id")]);
    let (mut sql, mut args) = sq.to_sql()?;
    let from = format!("FROM {TAG_TABLE}");
    let Some(pos) = sql.find(&from) else {
        return Err(crate::sqlrepo::SqlError::NoTable);
    };
    let at = pos + from.len();
    sql.replace_range(at..at, &join_sql);
    args.splice(..0, join_args.iter().cloned());
    Ok((sql, args))
}

/// Failures of the executed tag reads ([`TagRepo`]). The variant set follows the
/// sibling `RadioRepo::RadioError` (`radio_repo.rs:425-460`).
#[derive(Debug, thiserror::Error)]
pub enum TagError {
    /// The statement failed, or a row would not scan. There is no `PostScan` hook
    /// on this read (`grep -n "dbTag\|PostScan" persistence/sql_tags.go
    /// persistence/tag_repository.go` is empty), so this is a pure statement/scan
    /// failure; see [`crate::hydrate::row_to_tag`] for the aggregate aliases' NULL
    /// behavior.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// The statement could not be materialized — Go's `toSQL` failure
    /// (`sql_base_repository.go:358-372`).
    #[error(transparent)]
    Builder(#[from] crate::sqlrepo::SqlError),

    /// No row matched. Only [`TagRepo::read`] reports it: Go's `Read` runs
    /// `queryOne`, which maps `sql.ErrNoRows` to `model.ErrNotFound`
    /// (`sql_base_repository.go:375-388`, `dbx@v1.12.0/rows.go:237-244`). Executed
    /// (oracle `/tmp/sd3g1_oracle8`): a dictionary tag whose `library_tag` rows live
    /// only in an inaccessible library returns `data not found`, because the INNER
    /// `user_library` join removes the aggregate row entirely.
    ///
    /// [`TagRepo::get_all`] and [`TagRepo::get_all_plain`] NEVER report this
    /// variant: `queryAll`'s dbx `All` makes an empty non-nil slice for a zero-row
    /// result (`dbx@v1.12.0/query.go:278-291`), so its ErrNoRows arm is dead on that
    /// path. Executed: `GetAll("")` returned `len=0` with `err=<nil>`.
    #[error("data not found")]
    NotFound,
}

/// `tagRepository` (`persistence/tag_repository.go:20-24`) bound to one pooled
/// connection — the executed READ side of the tag dictionary.
///
/// Go takes the logged user from the ambient ctx (`loggedUser(ctx)`,
/// `sql_base_repository.go:49-58`); this crate has no ambient context (the
/// `sqlrepo.rs` DI freeze), so the caller passes the user id explicitly — `None` is
/// Go's `invalidUserId` headless context, which adds no `user_library` join
/// (`sql_tags.go:170-175`) — and every statement runs through [`crate::execution`],
/// the port of `queryOne`/`queryAll`/`count`
/// (`sql_base_repository.go:375-388,441-457,565-576`), in the shape
/// `radio_repo::RadioRepo` establishes.
///
/// The read is a single hydration phase ([`crate::hydrate::row_to_tag`]): no
/// wrapper, no `PostScan`, no shim column.
#[derive(Debug)]
pub struct TagRepo<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> TagRepo<'a> {
    /// `NewTagRepository` (`tag_repository.go:20-24`): bind to `conn` (the pooled
    /// lease). Go's `newBaseTagRepository(db, nil)` leaves the dictionary with no
    /// tag-name filter, so the name is a per-call argument here exactly as in
    /// `GetAll(ctx, name, …)`.
    #[must_use]
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// `GetAll` (`tag_repository.go:78-83`) with an empty option list.
    ///
    /// # Errors
    /// [`TagError::Sqlite`] on a statement or scan failure; [`TagError::Builder`] if
    /// the builder fails to materialize. Never [`TagError::NotFound`].
    pub fn get_all_plain(
        &self,
        tag_name: &str,
        user_id: Option<&str>,
    ) -> Result<TagList, TagError> {
        let statement = get_all_plain_sql(tag_name, user_id)?;
        Ok(crate::execution::query_all(
            self.conn,
            &statement,
            crate::hydrate::row_to_tag,
        )?)
    }

    /// `GetAll` (`tag_repository.go:78-83`) — the REST read (`baseTagRepository.
    /// ReadAll`, `sql_tags.go:223-228`): filters, the `buildSortOrder` leg over
    /// [`SORT_MAPPINGS`], and Max/Offset all reach the statement as they do in Go's
    /// `newSelect`, and this method PARSES its own rest options. Go's `ReadAll` calls
    /// `parseRestOptions` inside the method (`sql_tags.go:224`), so `sanitizeSort`
    /// (`sql_restful.go:69-86`) always runs before `applyOptions` renders the ORDER BY
    /// leg (`sql_base_repository.go:157-158`); here that is [`parse_rest_options`]
    /// feeding [`get_all_sql`]. A caller-supplied sort that is neither in
    /// [`SORT_MAPPINGS`] nor in [`rest_repo`]'s whitelist is dropped and contributes NO
    /// ORDER BY leg, so row order cannot act as an oracle on a column outside that
    /// whitelist — which matters here because the aggregate's `album_count` and
    /// `song_count` SUMs ARE whitelisted (`model/tag.go`'s `AlbumCount`/`SongCount`
    /// through `registerModelWhiteList` + `registerFieldWhiteList`,
    /// `sql_restful.go:142-163`) while an expression smuggled around them is not.
    ///
    /// `user_id` is Go's `loggedUser(ctx).ID` on the aggregate's INNER `user_library`
    /// leg (`sql_tags.go:171-174`); option parsing does not touch it, so the ownership
    /// scope stays exactly as it was.
    ///
    /// `options == None` is Go's empty variadic list: `parseRestOptions` returns the
    /// zero `model.QueryOptions` (`sql_restful.go:55-56,66`), i.e.
    /// [`crate::sql_restful::QueryOptions::default`] — no filter, no page, no ORDER BY,
    /// the same statement [`get_all_plain_sql`] renders.
    ///
    /// Deviation kept, not fixed: an `Offset` with no `Max` EXECUTES here
    /// (`LIMIT -1 OFFSET n`, `sqlrepo.rs:313-319`) where Go's bare `OFFSET n` is a
    /// SQLite parse error — executed Go returns `near "OFFSET": syntax error`
    /// (oracle `/tmp/sd3g1_oracle6`). Pinned rather than "fixed".
    ///
    /// # Errors
    /// [`TagError::Sqlite`] on a statement or scan failure; [`TagError::Builder`] if
    /// the builder fails to materialize. Never [`TagError::NotFound`].
    pub fn get_all(
        &self,
        tag_name: &str,
        user_id: Option<&str>,
        options: Option<&crate::sql_restful::RestQueryOptions>,
    ) -> Result<TagList, TagError> {
        let statement = get_all_sql(tag_name, user_id, Some(&parse_rest_options(options)), None)?;
        Ok(crate::execution::query_all(
            self.conn,
            &statement,
            crate::hydrate::row_to_tag,
        )?)
    }

    /// `baseTagRepository.Count` (`sql_tags.go:202-214`) under the caller's options:
    /// the DISTINCT aggregate.
    ///
    /// It deliberately KEEPS the [`crate::sql_restful::QueryOptions`] signature that
    /// [`Self::get_all`] abandoned. Go's `Count` builds its own
    /// `Select("COUNT(DISTINCT tag.id)")` (`sql_tags.go:203`) and never calls
    /// `applyOptions`, so a caller `Sort`/`Max`/`Offset` cannot reach this statement at
    /// all; `count` additionally drops the page (`sql_base_repository.go:568`) and
    /// installs `ORDER BY tag.id` (`:569`). It is not an ordering channel and carries no
    /// [`sanitize_sort`]-style leak — executed: `Count(Sort:"name", Order:"desc")`
    /// traced `… ORDER BY tag.id` alone (oracle `/tmp/sd3g1_oracle8`). Filters DO apply
    /// (`:571`), which is why they stay in the signature; the REST-shaped [`Self::count`]
    /// parses its options the way Go does at `sql_tags.go:213`.
    ///
    /// # Errors
    /// [`TagError::Sqlite`] when the statement fails; [`TagError::Builder`] if the
    /// builder fails to materialize. Go's `count` returns `res.Count` BESIDE a
    /// `queryOne` error (`sql_base_repository.go:572-574`); the port reports the
    /// error alone and the count defaults to 0.
    pub fn count_all(
        &self,
        user_id: Option<&str>,
        options: Option<&crate::sql_restful::QueryOptions>,
    ) -> Result<i64, TagError> {
        let statement = count_all_sql(user_id, options)?;
        Ok(
            crate::execution::query_optional(self.conn, &statement, |row| row.get::<_, i64>(0))?
                .unwrap_or_default(),
        )
    }

    /// `baseTagRepository.Count` (`sql_tags.go:202-214`) driven by REST options: Go's
    /// `count(ctx, sq, parseRestOptions(…))` (`:213`) in full. The whitelist check
    /// happens here even though the aggregate cannot be reordered by a caller sort;
    /// what this entry point really transports is the `name`/`library_id` filter
    /// mapping set (`sql_tags.go:155-158`).
    ///
    /// # Errors
    /// Same as [`Self::count_all`].
    pub fn count(
        &self,
        user_id: Option<&str>,
        options: Option<&crate::sql_restful::RestQueryOptions>,
    ) -> Result<i64, TagError> {
        self.count_all(user_id, Some(&parse_rest_options(options)))
    }

    /// `baseTagRepository.Read` (`sql_tags.go:216-221`): one aggregate row by id.
    ///
    /// # Errors
    /// [`TagError::NotFound`] when the aggregate has no row for the id (including a
    /// tag the user's libraries cannot see); [`TagError::Sqlite`] on a statement or
    /// scan failure; [`TagError::Builder`] if the builder fails to materialize.
    pub fn read(&self, user_id: Option<&str>, id: &str) -> Result<Tag, TagError> {
        let statement = read_sql(user_id, id)?;
        crate::execution::query_optional(self.conn, &statement, crate::hydrate::row_to_tag)?
            .ok_or(TagError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: &str, name: &str, value: &str) -> Tag {
        Tag {
            id: id.to_string(),
            tag_name: name.to_string(),
            tag_value: value.to_string(),
            album_count: 0,
            song_count: 0,
        }
    }

    #[test]
    fn add_emits_chunked_tag_and_library_tag_inserts() {
        // Arrange: Add's two statements for one chunk (tag_repository.go:26-44).
        let tags = vec![tag("t1", "genre", "Rock"), tag("t2", "genre", "Jazz")];
        // Act:
        let chunks = add_sql(3, &tags);
        // Assert: one chunk, dictionary insert first with 200-row cap respected.
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0].tag_insert.0,
            "INSERT INTO tag (id, tag_name, tag_value) VALUES (?, ?, ?), (?, ?, ?) on conflict (id) do nothing"
        );
        assert_eq!(
            chunks[0].tag_insert.1,
            vec![
                Value::Text("t1".to_string()),
                Value::Text("genre".to_string()),
                Value::Text("Rock".to_string()),
                Value::Text("t2".to_string()),
                Value::Text("genre".to_string()),
                Value::Text("Jazz".to_string()),
            ]
        );
        assert_eq!(
            chunks[0].library_tag_insert.0,
            "INSERT INTO library_tag (tag_id, library_id, album_count, media_file_count) VALUES (?, ?, 0, 0), (?, ?, 0, 0) on conflict (tag_id, library_id) do nothing"
        );
        assert_eq!(
            chunks[0].library_tag_insert.1,
            vec![
                Value::Text("t1".to_string()),
                Value::Integer(3),
                Value::Text("t2".to_string()),
                Value::Integer(3),
            ]
        );
    }

    #[test]
    fn add_splits_at_the_200_row_chunk_boundary() {
        // Arrange: slices.Chunk(tags, 200) (tag_repository.go:27).
        let tags = (0..201)
            .map(|i| tag(&format!("t{i}"), "genre", "Rock"))
            .collect::<Vec<_>>();
        // Act:
        let chunks = add_sql(1, &tags);
        // Assert: 201 tags → a 200-row chunk plus a 1-row chunk.
        assert_eq!(chunks.len(), 2);
        let full = &chunks[0].tag_insert.0;
        let rows = full.matches("), ").count() + 1;
        assert_eq!(rows, CHUNK_SIZE);
        assert!(chunks[1].tag_insert.0.matches("?, ?, ?").count() == 1);
    }

    #[test]
    fn update_counts_renders_both_table_upserts_with_genre_only_json_path() {
        // Arrange/Act: UpdateCounts' template per table (tag_repository.go:50-66).
        let stmts = update_counts_sql();
        // Assert: album then media_file, only the $.genre path aggregates.
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].0.contains("FROM album"));
        assert!(stmts[0].0.contains("json_tree(album.tags, '$.genre')"));
        assert!(stmts[0]
            .0
            .contains("DO UPDATE SET album_count = excluded.album_count;"));
        assert!(stmts[1].0.contains("FROM media_file"));
        assert!(stmts[1]
            .0
            .contains("DO UPDATE SET media_file_count = excluded.media_file_count;"));
        for (_, args) in &stmts {
            assert!(args.is_empty());
        }
    }

    #[test]
    fn get_all_binds_the_tag_name_and_scopes_the_sums_to_the_user() {
        // Arrange: GetAll + baseTagRepository.newSelect (tag_repository.go:78-83,
        // sql_tags.go:180-198) with a logged-in user.
        // Act:
        let (sql, args) = get_all_sql("genre", Some("user1"), None, None)
            .expect("the aggregate select materializes");
        // Assert: byte-identical to the executed Go trace (oracle
        // /tmp/sd3g1_oracle6), which renders the `library_tag` leg as LEFT JOIN
        // (`sq.LeftJoin`, sql_tags.go:168) and the `user_library` leg as a bare
        // INNER JOIN (`sq.Join`, sql_tags.go:173, rendered by squirrel
        // select.go:297-299). The earlier version of this test asserted LEFT JOIN
        // on both legs; the executed trace and an executed SQLite row-count
        // measurement (/tmp/sd3g_leftjoin: plain JOIN = 1, LEFT JOIN = 2) both
        // contradict that.
        assert_eq!(
            sql,
            "SELECT tag.id, tag.tag_name, tag.tag_value, \
             COALESCE(SUM(library_tag.album_count), 0) as album_count, \
             COALESCE(SUM(library_tag.media_file_count), 0) as song_count \
             FROM tag \
             LEFT JOIN library_tag on library_tag.tag_id = tag.id \
             JOIN user_library on user_library.library_id = library_tag.library_id AND user_library.user_id = ? \
             WHERE tag.tag_name = ? \
             GROUP BY tag.id, tag.tag_name, tag.tag_value"
        );
        // Assert: the JOIN arg binds ahead of the WHERE arg (squirrel binds join
        // parts before conditions), matching Go's args map p0=adm, p1=genre.
        assert_eq!(
            args,
            vec![
                Value::Text("user1".to_string()),
                Value::Text("genre".to_string())
            ]
        );
    }

    #[test]
    fn get_all_without_user_skips_the_user_library_join_and_honors_pagination() {
        // Arrange: no logged-in user (sql_tags.go:210-211) with a page.
        let options = crate::sql_restful::QueryOptions {
            max: 10,
            offset: 20,
            ..crate::sql_restful::QueryOptions::default()
        };
        // Act:
        let (sql, args) = get_all_sql("genre", None, Some(&options), None)
            .expect("the unscoped aggregate select materializes");
        // Assert:
        assert!(!sql.contains("user_library"));
        assert!(sql.ends_with(" GROUP BY tag.id, tag.tag_name, tag.tag_value LIMIT 10 OFFSET 20"));
        // Assert: the bounds are squirrel literals (`select.go:139-147`), so
        // only the tag type binds.
        assert_eq!(args, vec![Value::Text("genre".to_string())]);
    }

    #[test]
    fn purge_unused_deletes_unreferenced_dictionary_rows_without_bound_args() {
        // Arrange/Act: purgeUnused's delete (tag_repository.go:88-102).
        let (sql, args) = purge_unused_sql();
        // Assert:
        assert!(sql.starts_with("DELETE FROM tag WHERE id not in ("));
        assert!(sql.contains("from album left join json_tree(album.tags, '$')"));
        assert!(sql.contains("from media_file left join json_tree(media_file.tags, '$')"));
        assert!(args.is_empty());
    }
}
