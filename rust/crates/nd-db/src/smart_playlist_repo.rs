//! Port of `persistence/smart_playlist_repository.go` (205L) — the criteria
//! driven regeneration of a smart playlist's tracks.
//!
//! ## The criteria seam
//!
//! Go builds `smartPlaylistCriteria` (`persistence/criteria_sql.go`) — the
//! criteria→SQL compiler with the `smartPlaylistFields` map, the join planner
//! (`applyRequiredJoins`/`applyExpressionJoins`) and the `orderBy` resolver —
//! and consumes it here as `(where-Sql, order-by string, joins, limit)`.
//! **criteria_sql is slice 4's LAST lane** and is deliberately not ported in
//! this crate yet: the port lands the consumer side as a documented seam,
//! [`CriteriaSql`], that takes the already-built filter `Sql` (its rendered
//! WHERE fragment + args) plus the resolved order-by/joins from that lane.
//!
//! Every Go method builds a squirrel statement and hands it to
//! `executeSQL`/`queryOne`, which materialize through `toSQL()`
//! (`sql_base_repository.go:358-372`) as `(sql, args)`; execution and logging
//! stay with the caller (`store.rs`), so this port stops at the same [`Sql`]
//! pairs. The `userID`-embedded `annotation` LEFT JOINs are spelled out with a
//! bound `?` for the user id (`criteria_sql.go:838-841` binds it, unlike
//! `tracksQuery`'s concat), and the `playlist_id` literal in the INSERT-SELECT
//! is Go's own string splice (`smart_playlist_repository.go:175`) — the
//! id is a server-generated fixed identifier, never user text (slice-2
//! security contract).
//!
//! Not ported, with the reason: the tree recursion guard (`visited`,
//! `refreshSmartPlaylistTree`, `:27-45`) interleaves DB reads with mutation;
//! its child playlist resolution ([`child_playlist_filter_sql`]) is ported as
//! the select the caller runs per tree level. The artwork/counter follow-ups
//! (`refreshCounters`, `enqueueCoverRebuild`) are `playlist_repo.rs`'s — the
//! refresh ends at the evaluated_at stamp, the last statement Go runs.

use crate::sqlrepo::Value;

/// The `playlist` table (`registerModel(&model.Playlist{})`,
/// `playlist_repository.go:31`).
pub const TABLE: &str = "playlist";

/// The `playlist_tracks` table the refresh rewrites (`:42`).
pub const TRACKS_TABLE: &str = "playlist_tracks";

/// The `media_file` table every criteria query resolves against (`:165`).
pub const MEDIA_FILE_TABLE: &str = "media_file";

/// A materialized statement: `(sql, args)`, the `toSQL()`
/// (`sql_base_repository.go:358-372`) output shape.
pub type Sql = (String, Vec<Value>);

/// The criteria→SQL seam: what `smartPlaylistCriteria` contributes to the
/// refresh queries. The criteria compiler lane (slice 4's LAST lane,
/// `persistence/criteria_sql.go`) fills this in; the refresh consumer here
/// only needs the rendered filter, the order-by expression list, the join
/// fragments and the resolved limit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CriteriaSql {
    /// `cSQL.where()` (`criteria_sql.go:133`): the rendered WHERE fragment of
    /// the criteria expression, with its bound args.
    pub filter: Sql,
    /// `cSQL.orderBy()` (`criteria_sql.go:860-873`): the comma-joined
    /// order-by expressions over `smartPlaylistFields` — fixed identifiers
    /// from the field map, never user text (slice-2 security contract).
    pub order_by: String,
    /// `cSQL.applyRequiredJoins` (`criteria_sql.go:831-833`): the LEFT JOIN
    /// fragments (annotation/album) with their per-join bound args, in Go's
    /// declaration order. The unconditional `annotation` join is always
    /// present in the built queries and NOT repeated here.
    pub joins: Vec<Sql>,
    /// `cSQL.Criteria.Limit` after `ResolveLimit` (`criteria.go:50-58`).
    /// `0` means no limit (Go skips `LIMIT`/`OFFSET` then).
    pub limit: i64,
    /// `cSQL.Criteria.Offset` (`criteria.go:25`).
    pub offset: i64,
    /// `cSQL.Criteria.IsPercentageLimit()` (`criteria.go:62-64`): the
    /// percentage-limit flag that drives [`count_sql`].
    pub is_percentage_limit: bool,
}

/// The refresh statement sequence of `refreshSmartPlaylistTree`
/// (`smart_playlist_repository.go:27-90`), in Go's execution order: the
/// delete, then the INSERT-SELECT, then the counters update and the
/// evaluated_at stamp.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshSql {
    /// `DELETE FROM playlist_tracks WHERE playlist_id = ?` (`:42-45`).
    pub delete_tracks: Sql,
    /// `INSERT INTO playlist_tracks (id, playlist_id, media_file_id) SELECT
    /// …` (`:62-66` via `buildSmartPlaylistQuery`).
    pub insert_select: Sql,
    /// `playlist_repo.rs`'s `refresh_counters` pair, run between the insert
    /// and the stamp.
    pub refresh_counters: Vec<Sql>,
    /// `UPDATE playlist SET evaluated_at = ? WHERE id = ?` (`:73-75`).
    pub stamp_evaluated_at: Sql,
}

/// The annotation LEFT JOIN every criteria query carries
/// (`applyJoins`, `criteria_sql.go:838-841`) — bound, not concatenated:
/// `LEFT JOIN annotation ON (… AND annotation.user_id = ?)`.
#[must_use]
pub fn annotation_join(_user_id: &str) -> String {
    " LEFT JOIN annotation ON (annotation.item_id = media_file.id AND \
     annotation.item_type = 'media_file' AND annotation.user_id = ?)"
        .to_string()
}

/// The `annotation` join's bound args, in placeholder order.
#[must_use]
pub fn annotation_join_args(user_id: &str) -> Vec<Value> {
    vec![Value::Text(user_id.to_string())]
}

/// `refreshSmartPlaylistTree`'s statement sequence (`:42-78`), taking the
/// playlist id, the criteria seam value, the logged-in user id (Go embeds it
/// in every annotation join, `criteria_sql.go:841`), the resolved evaluation
/// timestamp (Go reuses `refreshCounters`'s `updated_at` stamp so both agree,
/// `:72-75`) and the caller-owned counter statements
/// (`playlist_repo.rs`).
///
/// `library_filter` is `applyLibraryFilter` (`sql_base_repository.go:261-283`):
/// `None` is the admin/full-access skip, `Some(user_id)` the subquery.
#[must_use]
pub fn refresh_sql(
    playlist_id: &str,
    criteria: &CriteriaSql,
    user_id: &str,
    evaluated_at: &str,
    library_filter: Option<&Sql>,
    refresh_counters: Vec<Sql>,
) -> RefreshSql {
    let delete_tracks = (
        format!("DELETE FROM {TRACKS_TABLE} WHERE playlist_id = ?"),
        vec![Value::Text(playlist_id.to_string())],
    );
    let insert_select = build_insert_select(playlist_id, criteria, user_id, library_filter);
    let stamp_evaluated_at = (
        format!("UPDATE {TABLE} SET evaluated_at = ? WHERE id = ?"),
        vec![
            Value::Text(evaluated_at.to_string()),
            Value::Text(playlist_id.to_string()),
        ],
    );
    RefreshSql {
        delete_tracks,
        insert_select,
        refresh_counters,
        stamp_evaluated_at,
    }
}

/// `buildSmartPlaylistQuery` + `addCriteria` + the INSERT-SELECT wrap
/// (`smart_playlist_repository.go:58-66, 176-196, 200-205`):
///
/// `INSERT INTO playlist_tracks (id, playlist_id, media_file_id)
///  SELECT row_number() over (order by <order_by>) as id,
///         '<playlist_id>' as playlist_id,
///         media_file.id as media_file_id
///  FROM media_file
///  <criteria joins>  <library filter>
///  WHERE <filter>
///  [ORDER BY <order_by>]  [LIMIT n [OFFSET n]]`
///
/// The `<playlist_id>` literal is Go's own string splice (`:175`); the port
/// keeps it as SQL text because the id is a server-generated fixed identifier.
/// `order_by` comes from the criteria seam's fixed field map — no user text
/// reaches the ORDER BY (slice-2 security contract).
#[must_use]
pub fn build_insert_select(
    playlist_id: &str,
    criteria: &CriteriaSql,
    user_id: &str,
    library_filter: Option<&Sql>,
) -> Sql {
    let mut args = Vec::new();
    let mut sql = format!(
        "INSERT INTO {TRACKS_TABLE} (id, playlist_id, media_file_id) \
         SELECT row_number() over (order by {order_by}) as id, '{playlist_id}' as playlist_id, \
         media_file.id as media_file_id FROM {MEDIA_FILE_TABLE}",
        order_by = criteria.order_by,
    );
    // The unconditional annotation join comes first (applyJoins,
    // criteria_sql.go:838-841), then the criteria's own join fragments.
    sql.push_str(&annotation_join(user_id));
    args.extend(annotation_join_args(user_id));
    for (join_sql, join_args) in &criteria.joins {
        sql.push(' ');
        sql.push_str(join_sql);
        args.extend(join_args.iter().cloned());
    }
    if let Some((filter_sql, filter_args)) = library_filter {
        sql.push_str(&format!(" WHERE {filter_sql}"));
        args.extend(filter_args.iter().cloned());
    }
    let (filter_sql, filter_args) = &criteria.filter;
    if !filter_sql.is_empty() {
        sql.push_str(if library_filter.is_some() {
            " AND "
        } else {
            " WHERE "
        });
        sql.push_str(filter_sql);
        args.extend(filter_args.iter().cloned());
    } else if library_filter.is_none() {
        // Go's bare Insert…Select has no WHERE then; match squirrel's shape.
        sql.push_str(" WHERE 1=1");
    }
    if !criteria.order_by.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", criteria.order_by));
    }
    if criteria.limit > 0 {
        // squirrel renders the bounds as decimal literals and contributes no
        // args (`select.go:139-147`). Go calls `.Limit().Offset()` together
        // inside the limit branch (:205), so both are emitted here too.
        sql.push_str(&format!(" LIMIT {}", criteria.limit));
        sql.push_str(&format!(" OFFSET {}", criteria.offset));
    }
    (sql, args)
}

/// `shouldRefreshSmartPlaylist` (`:95-109`): the refresh gate, spelled as
/// data — smart, outside its refresh-delay window, and owned by the user.
/// The caller owns the clock comparison (Go's `time.Since(EvaluatedAt)`).
/// `refresh_delay` is Go's `pls.RefreshDelay()` (`model/playlist.go:56-63`)
/// — the rules' own delay or the global default, pre-resolved by the caller.
#[must_use]
pub fn should_refresh(
    is_smart_playlist: bool,
    evaluated_at: Option<i64>,
    refresh_delay_secs: i64,
    since_evaluated_secs: i64,
    owner_id: &str,
    user_id: &str,
) -> bool {
    if !is_smart_playlist {
        return false;
    }
    if let Some(_evaluated) = evaluated_at {
        if since_evaluated_secs < refresh_delay_secs {
            return false;
        }
    }
    owner_id == user_id
}

/// `resolvePercentageLimit` (`:151-172`): the count query that converts a
/// percentage limit into an absolute one — `SELECT count(*) FROM media_file`
/// with the criteria's expression joins and the library filter, and the
/// criteria WHERE. The caller feeds the row count to the criteria seam's
/// `ResolveLimit` (nd-model's `resolve_limit`, `criteria.rs:122`).
#[must_use]
pub fn count_sql(criteria: &CriteriaSql, user_id: &str, library_filter: Option<&Sql>) -> Sql {
    let mut args = Vec::new();
    let mut sql = format!("SELECT count(*) as count FROM {MEDIA_FILE_TABLE}");
    sql.push_str(&annotation_join(user_id));
    args.extend(annotation_join_args(user_id));
    for (join_sql, join_args) in &criteria.joins {
        sql.push(' ');
        sql.push_str(join_sql);
        args.extend(join_args.iter().cloned());
    }
    let mut have_where = false;
    if let Some((filter_sql, filter_args)) = library_filter {
        sql.push_str(&format!(" WHERE {filter_sql}"));
        args.extend(filter_args.iter().cloned());
        have_where = true;
    }
    let (filter_sql, filter_args) = &criteria.filter;
    if !filter_sql.is_empty() {
        sql.push_str(if have_where { " AND " } else { " WHERE " });
        sql.push_str(filter_sql);
        args.extend(filter_args.iter().cloned());
    }
    (sql, args)
}

/// `refreshChildPlaylists`'s select (`:67-72`): the playlists a smart
/// playlist's rules reference by id or by path, in one OR of the two Eq
/// lists. `lookup_paths` are the caller's `pathVariants` expansions
/// (`:66-68`), normalized to NFC by the caller as Go does (`:88-96`).
#[must_use]
pub fn child_playlist_filter_sql(child_ids: &[String], lookup_paths: &[String]) -> Sql {
    let mut clauses = Vec::new();
    let mut args = Vec::new();
    if !child_ids.is_empty() {
        let placeholders = vec!["?"; child_ids.len()].join(", ");
        clauses.push(format!("playlist.id IN ({placeholders})"));
        args.extend(child_ids.iter().map(|id| Value::Text(id.clone())));
    }
    if !lookup_paths.is_empty() {
        let placeholders = vec!["?"; lookup_paths.len()].join(", ");
        clauses.push(format!("playlist.path IN ({placeholders})"));
        args.extend(lookup_paths.iter().map(|p| Value::Text(p.clone())));
    }
    (clauses.join(" OR "), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The criteria seam's fixture: one filter fragment, one order-by over
    /// the fixed field map, the album annotation join, a resolved limit.
    fn criteria_fixture() -> CriteriaSql {
        CriteriaSql {
            filter: (
                "media_file.title LIKE ?".to_string(),
                vec![Value::Text("%love%".to_string())],
            ),
            order_by: "media_file.title asc".to_string(),
            joins: vec![(
                "LEFT JOIN annotation AS album_annotation ON (album_annotation.item_id = \
                 media_file.album_id AND album_annotation.item_type = 'album' AND \
                 album_annotation.user_id = ?)"
                    .to_string(),
                vec![Value::Text("u1".to_string())],
            )],
            limit: 10,
            offset: 5,
            is_percentage_limit: false,
        }
    }

    #[test]
    fn refresh_pins_go_statement_sequence() {
        // Arrange: the refresh's four steps (:42-78), user "u1".
        let criteria = criteria_fixture();
        let lib = library_filter_fixture();
        // Act:
        let refresh = refresh_sql(
            "pl-9",
            &criteria,
            "u1",
            "2024-01-01 00:00:00+00:00",
            lib.as_ref(),
            vec![],
        );
        // Assert:
        assert_eq!(
            refresh.delete_tracks.0,
            "DELETE FROM playlist_tracks WHERE playlist_id = ?"
        );
        assert_eq!(
            refresh.delete_tracks.1,
            vec![Value::Text("pl-9".to_string())]
        );
        assert!(refresh.insert_select.0.starts_with(
            "INSERT INTO playlist_tracks (id, playlist_id, media_file_id) \
             SELECT row_number() over (order by media_file.title asc) as id, 'pl-9' as playlist_id, \
             media_file.id as media_file_id FROM media_file"
        ));
        assert_eq!(
            refresh.stamp_evaluated_at.0,
            "UPDATE playlist SET evaluated_at = ? WHERE id = ?"
        );
        assert_eq!(
            refresh.stamp_evaluated_at.1,
            vec![
                Value::Text("2024-01-01 00:00:00+00:00".to_string()),
                Value::Text("pl-9".to_string()),
            ]
        );
    }

    fn library_filter_fixture() -> Option<Sql> {
        Some((
            "media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?)"
                .to_string(),
            vec![Value::Text("u1".to_string())],
        ))
    }

    #[test]
    fn insert_select_binds_joins_filter_and_pagination_in_go_order() {
        // Arrange: buildSmartPlaylistQuery + addCriteria (:176-205).
        let criteria = criteria_fixture();
        let lib = library_filter_fixture();
        // Act:
        let (sql, args) = build_insert_select("pl-9", &criteria, "u1", lib.as_ref());
        // Assert:
        assert_eq!(
            sql,
            "INSERT INTO playlist_tracks (id, playlist_id, media_file_id) \
             SELECT row_number() over (order by media_file.title asc) as id, 'pl-9' as playlist_id, \
             media_file.id as media_file_id FROM media_file \
             LEFT JOIN annotation ON (annotation.item_id = media_file.id AND \
             annotation.item_type = 'media_file' AND annotation.user_id = ?) \
             LEFT JOIN annotation AS album_annotation ON (album_annotation.item_id = \
             media_file.album_id AND album_annotation.item_type = 'album' AND \
             album_annotation.user_id = ?) \
             WHERE media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?) \
             AND media_file.title LIKE ? ORDER BY media_file.title asc LIMIT 10 OFFSET 5"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("u1".to_string()),     // annotation join
                Value::Text("u1".to_string()),     // album_annotation join
                Value::Text("u1".to_string()),     // library subquery
                Value::Text("%love%".to_string()), // criteria filter
            ]
        );
        // Assert: the limit/offset bounds are squirrel literals
        // (`select.go:139-147`), so they contribute no args.
    }

    #[test]
    fn insert_select_without_a_limit_has_no_pagination_and_without_a_filter_has_1eq1() {
        // Arrange: Go skips LIMIT/OFFSET when the limit is 0 (:203-204) and
        // the criteria always contribute a WHERE (an empty criteria renders
        // a tautology — pinned here as the builder's).
        let mut criteria = criteria_fixture();
        criteria.limit = 0;
        criteria.joins.clear();
        criteria.filter = (String::new(), vec![]);
        // Act:
        let (sql, args) = build_insert_select("pl-9", &criteria, "u1", None);
        // Assert:
        assert!(sql.contains("WHERE 1=1"));
        assert!(!sql.contains("LIMIT"));
        assert!(!sql.contains("OFFSET"));
        assert_eq!(args, vec![Value::Text("u1".to_string())]);
    }

    #[test]
    fn count_sql_counts_matching_tracks_with_the_criteria_joins() {
        // Arrange: resolvePercentageLimit's count query (:151-172).
        let criteria = criteria_fixture();
        let lib = library_filter_fixture();
        // Act:
        let (sql, args) = count_sql(&criteria, "u1", lib.as_ref());
        // Assert:
        assert_eq!(
            sql,
            "SELECT count(*) as count FROM media_file \
             LEFT JOIN annotation ON (annotation.item_id = media_file.id AND \
             annotation.item_type = 'media_file' AND annotation.user_id = ?) \
             LEFT JOIN annotation AS album_annotation ON (album_annotation.item_id = \
             media_file.album_id AND album_annotation.item_type = 'album' AND \
             album_annotation.user_id = ?) \
             WHERE media_file.library_id IN (SELECT ul.library_id FROM user_library ul WHERE ul.user_id = ?) \
             AND media_file.title LIKE ?"
        );
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn should_refresh_gates_on_smart_delay_and_ownership() {
        // Arrange: shouldRefreshSmartPlaylist (:95-109).
        // Act/Assert: not smart → never.
        assert!(!should_refresh(false, None, 0, 0, "u1", "u1"));
        // Inside the delay window → no.
        assert!(!should_refresh(true, Some(100), 60, 10, "u1", "u1"));
        // Outside it → yes.
        assert!(should_refresh(true, Some(100), 60, 61, "u1", "u1"));
        // Never evaluated → yes.
        assert!(should_refresh(true, None, 60, 0, "u1", "u1"));
        // Another owner → no.
        assert!(!should_refresh(true, None, 60, 0, "u2", "u1"));
    }

    #[test]
    fn child_playlist_filter_ors_ids_and_paths() {
        // Arrange: refreshChildPlaylists' conditions (:60-68).
        let ids = vec!["pl-1".to_string(), "pl-2".to_string()];
        let paths = vec!["/music/x.nsp".to_string()];
        // Act:
        let both = child_playlist_filter_sql(&ids, &paths);
        let only_ids = child_playlist_filter_sql(&ids, &[]);
        // Assert:
        assert_eq!(both.0, "playlist.id IN (?, ?) OR playlist.path IN (?)");
        assert_eq!(
            both.1,
            vec![
                Value::Text("pl-1".to_string()),
                Value::Text("pl-2".to_string()),
                Value::Text("/music/x.nsp".to_string()),
            ]
        );
        assert_eq!(only_ids.0, "playlist.id IN (?, ?)");
    }
}
