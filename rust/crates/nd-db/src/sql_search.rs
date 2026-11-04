//! Port of `persistence/sql_search.go` (142L) — the shared search contract.
//!
//! What lives here is the dispatch + two-phase rowid-page machinery every text
//! search family funnels through: `doSearch`, `executeTwoPhase`,
//! `hydrateRowidPage` and `mbidExpr` (line refs given per item below).
//! Execution/logging stay out of scope, as in the sibling `sql_*` ports: the
//! ports stop at `(sql, args)` pairs (squirrel `ToSql()`, re-emitted by
//! [`crate::sqlrepo::Repository::to_sql`]) rather than binding them.
//!
//! Deliberately absent — owned by other units/lanes, nothing here can stand in
//! for them:
//! - `formatFullText` (`sql_search.go:15`) wraps `utils/str` `SanitizeStrings`
//!   (accent folding, `Clear`, ignored-char regexes): that machinery is not in
//!   this crate, so the helper ports with the code that writes `full_text`.
//! - `newLegacySearch`/`newLikeSearch`/`newFTSSearch`, `containsCJK` and
//!   `buildFTS5Query` live in `sql_search_like.rs`/`sql_search_fts.rs` —
//!   currently stubs of sibling lanes. So [`do_search`] cannot call Go's
//!   `getSearchStrategy` (`sql_search.go:27-33`); the strategy the sibling
//!   lanes chose arrives as an argument through the [`SearchStrategy`] seam.
//!   Seam: when `sql_search_fts.rs` lands, its `buildFTS5Query` product feeds
//!   [`SearchStrategy`] here — nothing in this file tokenizes user text.
//! - `r.applyLibraryFilter` (`sql_base_repository.go:214-217`) is a base-repo
//!   method; [`do_search`] takes the already-resolved clause instead
//!   (mirroring `cfg.LibraryFilter`, `sql_search.go:16-17`).

use crate::sql_restful::QueryOptions;
use crate::sqlrepo::{Repository, SqlError, Value};

/// A materialized statement or clause: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// `searchConfig` (`sql_search.go:9-24`). Go's `LibraryFilter` closure field is
/// a caller-supplied clause argument on the phase functions instead.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchConfig {
    /// ORDER BY for empty-query results (Go's `NaturalOrder`, e.g. `"album.rowid"`).
    natural_order: String,
    /// ORDER BY for text-search results (Go's `OrderBy`, e.g. `["name"]`).
    order_by: Vec<String>,
    /// Columns matched when the query is a UUID (Go's `MBIDFields`).
    mbid_fields: Vec<String>,
}

/// `searchStrategy` (`sql_search.go:21-25`) as data. `clause` is the strategy's
/// `ToSql()` (the single-query fallback shape `sql_search_fts.go:266-271` and
/// `sql_search_like.go:13-17` produce); `rowid_core` is the strategy-specific
/// Phase-1 builder its `execute()` contributes (`sql_search_fts.go:278-286`).
/// `None` rowid_core marks the LIKE family, which has no two-phase split
/// (`sql_search_like.go:16-24` queries the full SELECT directly).
#[derive(Debug, Clone)]
pub struct SearchStrategy {
    clause: Sql,
    rowid_core: Option<Repository>,
}

/// `doSearch`'s outcome. Go executes and fills `results`; the port names the
/// observable shapes: a query to bind, or an empty result (`sql_search.go:59-61`
/// min-length guard, `:47-49` nil strategy).
#[derive(Debug, Clone, PartialEq)]
pub enum SearchOutcome {
    /// `sql_search.go:59` (`len(q) < 2`) or `:47` (nil strategy): return no rows.
    NoResults,
    /// A materialized query plus its bound args, in bind order.
    Query(Sql),
}

/// Canonical 8-4-4-4-12 hex UUID check, standing in for `uuid.Validate`
/// (`sql_search.go:55` and `:131`): the repositories store MBIDs in that form,
/// so Go's other accepted spellings (`urn:uuid:`, braces) never reach this path.
fn is_valid_uuid(s: &str) -> bool {
    const WIDTHS: [usize; 5] = [8, 4, 4, 4, 12];
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let mut pos = 0;
    for width in WIDTHS.iter().copied() {
        for offset in 0..width {
            if !bytes[pos + offset].is_ascii_hexdigit() {
                return false;
            }
        }
        pos += width;
        if pos < 36 {
            if bytes[pos] != b'-' {
                return false;
            }
            pos += 1;
        }
    }
    true
}

/// `mbidExpr` (`sql_search.go:126-141`): the OR of `<table>.<mbidField> = ?`
/// clauses with the lower-cased MBID bound as parameters. Go returns a nil
/// `Sqlizer` for a non-UUID query or an empty field list — both are `None`
/// here. The group is parenthesized because squirrel renders `Or(...)` as a
/// parenthesized list; unparenthesized it would bind wrongly once AND-joined.
#[must_use]
pub fn mbid_expr(table: &str, mbid: &str, mbid_fields: Vec<String>) -> Option<Sql> {
    if mbid_fields.is_empty() || !is_valid_uuid(mbid) {
        return None;
    }
    let mbid = mbid.to_lowercase();
    let clauses = mbid_fields
        .iter()
        .map(|field| format!("{table}.{field} = ?"))
        .collect::<Vec<String>>()
        .join(" OR ");
    let args = vec![Value::from(mbid); mbid_fields.len()];
    Some((format!("({clauses})"), args))
}

/// `newSelect(ctx, options...)` as the search layer uses it
/// (`sql_search.go:36` — `sq` arrives already built this way;
/// `sql_base_repository.go:139` + `applyOptions` `:145-153`): start the SELECT
/// on `table` and apply LIMIT/OFFSET. The sort→ORDER BY half needs the base
/// repository's `sortMappings`/`buildSortOrder`, so callers hand resolved
/// order expressions to [`Repository::order_by`] instead of setting them here.
#[must_use]
pub fn new_select(table: &str, options: Option<&QueryOptions>) -> Repository {
    let sq = Repository::new(table);
    match options {
        Some(o) => apply_pagination(sq, o),
        None => sq,
    }
}

/// `applyOptions`' LIMIT/OFFSET half (`sql_base_repository.go:145-153`): a
/// non-positive bound leaves the clause unset, matching Go's `> 0` guards.
#[must_use]
pub fn apply_pagination(sq: Repository, options: &QueryOptions) -> Repository {
    let sq = if options.max > 0 {
        sq.limit(u64::try_from(options.max).unwrap_or(u64::MAX))
    } else {
        sq
    };
    if options.offset > 0 {
        sq.offset(u64::try_from(options.offset).unwrap_or(u64::MAX))
    } else {
        sq
    }
}

/// `options.Filters` applied to a builder (`sql_search.go:98-100`) via
/// [`QueryOptions::where_clause`]: filters are caller-constructed SQL over
/// registered columns with bound values, hence the raw fragment.
#[must_use]
pub fn apply_filters(sq: Repository, options: &QueryOptions) -> Repository {
    match options.where_clause() {
        None => sq,
        Some((sql, args)) => sq.where_raw(sql, args),
    }
}

/// `hydrateRowidPage` (`sql_search.go:108-121` in Go's declaration order, body
/// at `:126-141`): strip `sq`'s pagination (`RemoveLimit`/`RemoveOffset`,
/// `:126-127`) — then join `sq` to the ranked rowid subquery and order by its
/// row number so Phase 2 preserves Phase 1's order. The rowid query's args bind
/// AT THE JOIN POSITION (`:141`
/// `sq.Join(rankedSubquery+" ON "+r.tableName+".rowid = _ranked._rid", rowidArgs...)`),
/// i.e. before `sq`'s own WHERE args — which is how user text reaches SQLite
/// as a `?` PARAM, never interpolated into the fragment.
pub fn hydrate_rowid_page(
    sq: Repository,
    table: &str,
    rowid_query: Repository,
) -> Result<Sql, SqlError> {
    let (rowid_sql, rowid_args) = rowid_query.to_sql()?;
    let ranked = format!(
        "(SELECT rowid as _rid, row_number() OVER () AS _rn FROM ({rowid_sql})) AS _ranked"
    );
    let hydrated = sq
        .remove_limit()
        .remove_offset()
        .join(ranked, format!("{table}.rowid = _ranked._rid"), rowid_args)
        .order_by(["_ranked._rn"]);
    hydrated.to_sql()
}

/// `executeTwoPhase`'s Phase 1 (`sql_search.go:66-100`): the shared search
/// contract over a strategy-specific rowid core — non-missing rows
/// (`sq.Where(Eq{missing: false})`, `:79`), pagination (`:81-87`), library
/// access (`:89-93`), `options.Filters` (`:98-100`) — applied in Go's order so
/// the bound-args order is stable. `library_filter` is `cfg.LibraryFilter` when
/// the entity routes through a junction table (`sql_search.go:89`), else the
/// base repository's `applyLibraryFilter` clause resolved by the caller.
#[must_use]
pub fn rowid_page_phase(
    rowid_core: Repository,
    table: &str,
    options: &QueryOptions,
    library_filter: Option<Sql>,
) -> Repository {
    let sq = rowid_core.where_eq(format!("{table}.missing"), false);
    let sq = apply_pagination(sq, options);
    let sq = match library_filter {
        Some((sql, args)) => sq.where_raw(sql, args),
        None => sq,
    };
    apply_filters(sq, options)
}

/// `executeTwoPhase` (`sql_search.go:66-100`): run the Phase-1 rowid query and
/// hydrate its page. Split into [`rowid_page_phase`] + [`hydrate_rowid_page`]
/// so callers can inspect each phase's SQL separately (Go's split is two
/// function calls; the covering-index comment at `:66-73` is why the phases
/// exist at all).
pub fn execute_two_phase(
    sq: Repository,
    table: &str,
    rowid_core: Repository,
    options: &QueryOptions,
    library_filter: Option<Sql>,
) -> Result<Sql, SqlError> {
    let rowid_query = rowid_page_phase(rowid_core, table, options, library_filter);
    hydrate_rowid_page(sq, table, rowid_query)
}

/// The count query for a search page (`sql_base_repository.go:570-590`
/// `count()`): `count(distinct <table>.id) as count` over the filtered select,
/// keeping the joins/filters [`Repository::count`] retains.
pub fn count_sql(sq: Repository, table: &str) -> Result<Sql, SqlError> {
    sq.count(table).to_sql()
}

/// `doSearch` (`sql_search.go:35-62`): trim, strip a trailing `*`, add the
/// non-missing restriction (`sq.Where(Eq{missing: false})`, `:44`), then
/// dispatch: empty query → natural-order rowid page (`:47-50`), valid UUID +
/// MBID fields → MBID match (`:54-57`), sub-two-character query → nothing
/// (`:59-61`, byte length as in Go), otherwise the strategy the sibling lanes
/// chose. `strategy` is Go's `getSearchStrategy` result (`:43-46`): `None`
/// means no searchable tokens.
pub fn do_search(
    table: &str,
    sq: Repository,
    query: &str,
    cfg: &SearchConfig,
    options: Option<&QueryOptions>,
    library_filter: Option<Sql>,
    strategy: Option<SearchStrategy>,
) -> Result<SearchOutcome, SqlError> {
    let trimmed = query.trim();
    let query = trimmed.strip_suffix('*').unwrap_or(trimmed);
    let select = sq.where_eq(format!("{table}.missing"), false);
    let empty_options = QueryOptions::default();

    if query.is_empty() || query == "\"\"" {
        let rowid_core = Repository::new(table)
            .select([format!("{table}.rowid")])
            .order_by([cfg.natural_order.clone()]);
        let page = execute_two_phase(
            select,
            table,
            rowid_core,
            options.unwrap_or(&empty_options),
            library_filter,
        );
        return Ok(SearchOutcome::Query(page?));
    }

    if let Some((sql, args)) = mbid_expr(table, query, cfg.mbid_fields.clone()) {
        let with_mbid = select.where_raw(sql, args);
        return Ok(SearchOutcome::Query(with_mbid.to_sql()?));
    }

    // Go's `len(q) < 2` counts UTF-8 bytes; Rust's `str::len` does too.
    if query.len() < 2 {
        return Ok(SearchOutcome::NoResults);
    }

    match strategy {
        None => Ok(SearchOutcome::NoResults),
        Some(strategy) => match strategy {
            SearchStrategy {
                clause: (_, _),
                rowid_core: Some(core),
            } => Ok(SearchOutcome::Query(execute_two_phase(
                select,
                table,
                core,
                options.unwrap_or(&empty_options),
                library_filter,
            )?)),
            // likeSearch.execute (sql_search_like.go:16-24): the clause filters the
            // full SELECT and cfg.OrderBy orders it — no rowid page.
            SearchStrategy {
                clause: (sql, args),
                rowid_core: None,
            } => Ok(SearchOutcome::Query(
                select
                    .where_raw(sql, args)
                    .order_by(cfg.order_by.clone())
                    .to_sql()?,
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_album() -> SearchConfig {
        SearchConfig {
            natural_order: "album.rowid".to_string(),
            order_by: vec!["name".to_string()],
            mbid_fields: vec![
                "mbid_album_id".to_string(),
                "mbid_release_group_id".to_string(),
            ],
        }
    }

    #[test]
    fn hydrate_rowid_page_pins_go_join_shape_and_arg_order() {
        // Arrange: hydrateRowidPage (sql_search.go:126-141). Phase 1's query
        // has its own args (the library user + LIMIT/OFFSET); sq carries a
        // user filter. The join args must bind BEFORE sq's WHERE args.
        let rowid_query = Repository::new("album")
            .select(["album.rowid"])
            .where_eq("album.missing", false)
            .where_raw(
                "album.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)".to_string(),
                vec![Value::from("user1")],
            )
            .order_by(["album.rowid"])
            .limit(10)
            .offset(20);
        let sq = Repository::new("album")
            .where_eq("album.name", "Abbey Road".to_string())
            .limit(5)
            .offset(50);
        // Act:
        let (sql, args) = hydrate_rowid_page(sq, "album", rowid_query).expect("rowid page renders");
        // Assert: LIMIT/OFFSET stripped from sq (RemoveLimit/RemoveOffset),
        // the _ranked subquery inlined, order preserved via _ranked._rn, and
        // args are [rowid-query args..., sq's WHERE args...] — user text is a
        // `?` param, never interpolated.
        assert_eq!(
            sql,
            "SELECT * FROM album \
             INNER JOIN (SELECT rowid as _rid, row_number() OVER () AS _rn FROM \
             (SELECT album.rowid FROM album WHERE album.missing = ? AND \
             album.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?) \
             ORDER BY album.rowid LIMIT ? OFFSET ?)) AS _ranked ON album.rowid = _ranked._rid \
             WHERE album.name = ? ORDER BY _ranked._rn"
        );
        assert_eq!(
            args,
            vec![
                Value::Bool(false),
                Value::Text("user1".to_string()),
                Value::Integer(10),
                Value::Integer(20),
                Value::Text("Abbey Road".to_string()),
            ]
        );
    }

    #[test]
    fn do_search_empty_query_pins_natural_order_rowid_page() {
        // Arrange: doSearch (sql_search.go:47-50) for search3 query="". The
        // full select keeps its pagination? No — hydrate strips it (sq's
        // LIMIT ? OFFSET ? below belong to the ROWID query, not sq).
        let options = QueryOptions {
            max: 20,
            offset: 40,
            ..QueryOptions::default()
        };
        let library_filter: Option<Sql> = Some((
            "album.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::from("user1")],
        ));
        let sq = new_select("album", Some(&options));
        // Act:
        let outcome = do_search(
            "album",
            sq,
            "",
            &cfg_album(),
            Some(&options),
            library_filter,
            None,
        )
        .expect("empty query renders");
        // Assert: the two phases are exactly Phase 1 (rowid + missing +
        // library + pagination) hydrated into Phase 2 with no LIMIT of its own.
        let SearchOutcome::Query((sql, args)) = outcome else {
            panic!("empty query must produce a query");
        };
        assert_eq!(
            sql,
            "SELECT * FROM album \
             INNER JOIN (SELECT rowid as _rid, row_number() OVER () AS _rn FROM \
             (SELECT album.rowid FROM album WHERE album.missing = ? AND \
             album.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?) \
             ORDER BY album.rowid LIMIT ? OFFSET ?)) AS _ranked ON album.rowid = _ranked._rid \
             WHERE album.missing = ? ORDER BY _ranked._rn"
        );
        assert_eq!(
            args,
            vec![
                Value::Bool(false),
                Value::Text("user1".to_string()),
                Value::Integer(20),
                Value::Integer(40),
                Value::Bool(false),
            ]
        );
    }

    #[test]
    fn do_search_mbid_query_binds_lowercased_uuid_per_field() {
        // Arrange: doSearch (sql_search.go:54-57) via mbidExpr (:126-141):
        // valid UUID → OR over the config's MBID fields, lower-cased, each
        // bound as a param.
        let cfg = cfg_album();
        let sq = new_select("album", None);
        // Act:
        let outcome = do_search(
            "album",
            sq,
            "F76D3C25-3B6B-4C61-83C7-8E0A0B6D0A47",
            &cfg,
            None,
            None,
            None,
        )
        .expect("mbid query renders");
        // Assert:
        let SearchOutcome::Query((sql, args)) = outcome else {
            panic!("UUID query must produce a query");
        };
        assert_eq!(
            sql,
            "SELECT * FROM album WHERE album.missing = ? AND \
             (album.mbid_album_id = ? OR album.mbid_release_group_id = ?)"
        );
        assert_eq!(
            args,
            vec![
                Value::Bool(false),
                Value::Text("f76d3c25-3b6b-4c61-83c7-8e0a0b6d0a47".to_string()),
                Value::Text("f76d3c25-3b6b-4c61-83c7-8e0a0b6d0a47".to_string()),
            ]
        );
    }

    #[test]
    fn do_search_single_char_and_nil_strategy_return_no_results() {
        // Arrange: sql_search.go:59-61 (min length, bytes) and :47-49 (nil
        // strategy). Trimming/stripping happens first (:41-42).
        let cfg = cfg_album();
        // Act:
        let short = do_search(
            "album",
            new_select("album", None),
            "a*",
            &cfg,
            None,
            None,
            None,
        )
        .expect("short query resolves");
        let nil_strategy = do_search(
            "album",
            new_select("album", None),
            "ab",
            &cfg,
            None,
            None,
            None,
        )
        .expect("nil strategy resolves");
        // Assert:
        assert_eq!(short, SearchOutcome::NoResults);
        assert_eq!(nil_strategy, SearchOutcome::NoResults);
    }

    #[test]
    fn do_search_like_strategy_filters_full_select_without_rowid_page() {
        // Arrange: likeSearch.execute (sql_search_like.go:16-24) — clause onto
        // the full SELECT, cfg.OrderBy, no two-phase split. The clause shape
        // itself is sql_search_like.rs's (sibling lane); here only the routing
        // is pinned.
        let cfg = cfg_album();
        let strategy = SearchStrategy {
            clause: (
                "album.name LIKE ?".to_string(),
                vec![Value::Text("%beatles%".to_string())],
            ),
            rowid_core: None,
        };
        // Act:
        let outcome = do_search(
            "album",
            new_select("album", None),
            "beatles",
            &cfg,
            None,
            None,
            Some(strategy),
        )
        .expect("like strategy renders");
        // Assert:
        let SearchOutcome::Query((sql, args)) = outcome else {
            panic!("LIKE query must produce a query");
        };
        assert_eq!(
            sql,
            "SELECT * FROM album WHERE album.missing = ? AND album.name LIKE ? ORDER BY name"
        );
        assert_eq!(
            args,
            vec![Value::Bool(false), Value::Text("%beatles%".to_string())]
        );
    }

    #[test]
    fn do_search_fts_strategy_hydrates_rowid_page_with_join_args_first() {
        // Arrange: ftsSearch.execute (sql_search_fts.go:278-286): Phase 1
        // joins the FTS table with `MATCH ?` — user text is the join arg and
        // MUST stay a param; the built clause (ftsSearch.ToSql, :266-271) is
        // unused on this path. rowid_core is what the sibling lane's
        // newFTSSearch produces.
        let cfg = cfg_album();
        let options = QueryOptions {
            max: 10,
            ..QueryOptions::default()
        };
        let rowid_core = Repository::new("album")
            .select(["album.rowid"])
            .join(
                "album_fts".to_string(),
                "album_fts.rowid = album.rowid AND album_fts MATCH ?".to_string(),
                vec![Value::Text("(beatles OR beatles*)".to_string())],
            )
            .order_by(["album_fts.rank".to_string(), "album.name".to_string()]);
        let strategy = SearchStrategy {
            clause: (
                "album.rowid IN (SELECT rowid FROM album_fts WHERE album_fts MATCH ?)".to_string(),
                vec![Value::Text("(beatles OR beatles*)".to_string())],
            ),
            rowid_core: Some(rowid_core),
        };
        let sq = new_select("album", Some(&options));
        // Act:
        let outcome = do_search(
            "album",
            sq,
            "beatles",
            &cfg,
            Some(&options),
            None,
            Some(strategy),
        )
        .expect("fts strategy renders");
        // Assert: the MATCH param binds at the JOIN position (before sq's
        // missing = ? clause — args prove the ordering contract).
        let SearchOutcome::Query((sql, args)) = outcome else {
            panic!("FTS query must produce a query");
        };
        assert_eq!(
            sql,
            "SELECT * FROM album \
             INNER JOIN (SELECT rowid as _rid, row_number() OVER () AS _rn FROM \
             (SELECT album.rowid FROM album INNER JOIN album_fts ON \
             album_fts.rowid = album.rowid AND album_fts MATCH ? WHERE \
             album.missing = ? ORDER BY album_fts.rank, album.name LIMIT ?)) AS _ranked ON album.rowid = _ranked._rid \
             WHERE album.missing = ? ORDER BY _ranked._rn"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("(beatles OR beatles*)".to_string()),
                Value::Bool(false),
                Value::Integer(10),
                Value::Bool(false),
            ]
        );
    }

    #[test]
    fn mbid_expr_rejects_non_uuid_and_empty_fields() {
        // Arrange: mbidExpr's nil-Sqlizer cases (sql_search.go:131, :127).
        // Act + Assert:
        assert_eq!(
            mbid_expr("album", "not-a-uuid", vec!["mbid_album_id".to_string()]),
            None
        );
        assert_eq!(
            mbid_expr("album", "f76d3c25-3b6b-4c61-83c7-8e0a0b6d0a47", vec![]),
            None
        );
        // 35 chars (missing one hex digit) is not a UUID.
        assert_eq!(
            mbid_expr(
                "album",
                "f76d3c25-3b6b-4c61-83c7-8e0a0b6d0a4",
                vec!["x".to_string()]
            ),
            None
        );
        assert!(mbid_expr(
            "album",
            "f76d3c25-3b6b-4c61-83c7-8e0a0b6d0a47",
            vec!["mbid_album_id".to_string()]
        )
        .is_some());
    }

    #[test]
    fn count_sql_pins_count_shape_over_filtered_select() {
        // Arrange: the count query a search page answers with
        // (sql_base_repository.go:570-590): distinct ids, pagination dropped.
        let sq = Repository::new("album")
            .where_eq("album.missing", false)
            .limit(10)
            .offset(5);
        // Act:
        let (sql, args) = count_sql(sq, "album").expect("count renders");
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(distinct album.id) as count FROM album WHERE album.missing = ?"
        );
        assert_eq!(args, vec![Value::Bool(false)]);
    }
}
