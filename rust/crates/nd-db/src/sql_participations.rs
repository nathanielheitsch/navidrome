//! Port of `persistence/sql_participations.go` (146L) — participant filter
//! SQL shaping. The marshalling half (`marshalParticipants`, `:41-54`;
//! `unmarshalParticipants`, `:56-76`) and the row mutation
//! (`updateParticipants`, `:93-137`; `getParticipants`, `:139-146`) run over
//! `model.Participants`, which nd-model does not define yet (see
//! `rust/crates/nd-model/src/album.rs:6` — "Not modelled (separate #06
//! modules): `Tags`, `Participants`"); only the pure-SQL half ports here,
//! and the rest is deferred to that #06 slice rather than stubbed.
//!
//! [`participant_id_filter`]/[`not_participant_id_filter`] model
//! `ParticipantIDFilter`/`NotParticipantIDFilter` (`:21-37`): a semi-join
//! `<table>_artists` subquery — `json_tree` over the JSON is far slower at
//! scale (`:19-20`) — negated with `NOT IN` for the negative filter.
//!
//! Squirrel renders `Eq{artist_id}` as `artist_id = ?` and the role
//! `Eq`-slice as `role IN (?, …)` (`:29-30`); an empty-role slice is
//! unreachable Go-side (len(roles) > 0 guard, `:29`), so the Rust
//! `roles: &[&str]` overload port keeps the same `IN`-rendering
//! specialization for arity as Go's always-nonempty branch — see
//! the tests.
//!
//! Safety rule (see `sqlrepo.rs`): the `<table>` identifier is
//! caller-supplied per the Go API contract (`:19` "matches rows of table");
//! `artist_id` is a bound `?` value.

use crate::sqlrepo::Value;

/// The `<table>` + `_id`/`_artists` suffixes of `participantIDFilter`
/// (`persistence/sql_participations.go:33-36`).
/// The `_artists` join table suffix (`participantIDFilter`'s FROM table,
/// `sql_participations.go:34`).
pub const ARTISTS_TABLE_SUFFIX: &str = "_artists";
/// The `_id` suffix of `<table>_id` (`participantIDFilter`'s selected
/// column, `sql_participations.go:33`).
pub const ID_COLUMN_SUFFIX: &str = "_id";

/// The `<table>.id` column `ParticipantIDFilter` matches
/// (`sql_participations.go:35`).
#[must_use]
pub fn participant_id_column(table: &str) -> String {
    format!("{table}.id")
}

/// The `SELECT <table>_id FROM <table>_artists WHERE artist_id = ?` subquery
/// of `participantIDFilter` (`sql_participations.go:33-36`), plus its bound
/// args; add the role filter with [`with_participant_roles`] when roles are
/// given (`:29-31`).
pub fn participant_ids_subquery(table: &str, artist_id: &str) -> (String, Vec<Value>) {
    (
        format!(
            "SELECT {table}{ID_COLUMN_SUFFIX} FROM {table}{ARTISTS_TABLE_SUFFIX} WHERE artist_id = ?"
        ),
        vec![Value::from(artist_id)],
    )
}

/// Appends `AND role IN (?, …)` to a [`participant_ids_subquery`] shape
/// (`sql_participations.go:29-31`, nonempty roles branch), returning the new
/// fragment and full arg list.
///
/// # Panics
/// On an empty `roles` slice — Go guards `len(roles) > 0` before reaching the
/// `Eq` (`:29`); reaching the empty case here is a caller bug, and silently
/// dropping the role filter would widen the match.
#[must_use]
pub fn with_participant_roles(
    table: &str,
    artist_id: &str,
    roles: &[&str],
) -> (String, Vec<Value>) {
    assert!(!roles.is_empty(), "roles must not be empty (see :29-31)");
    let placeholders = vec!["?"; roles.len()].join(", ");
    let mut args = vec![Value::from(artist_id)];
    args.extend(roles.iter().map(|role| Value::from(*role)));
    (
        format!(
            "SELECT {table}{ID_COLUMN_SUFFIX} FROM {table}{ARTISTS_TABLE_SUFFIX} \
             WHERE artist_id = ? AND role IN ({placeholders})"
        ),
        args,
    )
}

/// `ParticipantIDFilter` (`sql_participations.go:21-24`): the positive
/// semi-join — `… AND <table>.id IN (<subquery>)`.
#[must_use]
pub fn participant_id_filter(table: &str, artist_id: &str, roles: &[&str]) -> (String, Vec<Value>) {
    finish_participant_filter(table, artist_id, roles, false)
}

/// `NotParticipantIDFilter` (`sql_participations.go:26-28`): the negated
/// semi-join — `… AND <table>.id NOT IN (<subquery>)`.
#[must_use]
pub fn not_participant_id_filter(
    table: &str,
    artist_id: &str,
    roles: &[&str],
) -> (String, Vec<Value>) {
    finish_participant_filter(table, artist_id, roles, true)
}

/// `participantIDFilter` (`sql_participations.go:32-38`): renders the subquery
/// with its optional role filter, then wraps it in
/// `expr + op + sql + ")"` — the full Go shape `table+".id"+op+sql+")"`
/// (`:36`) with `op = " IN ("` / `" NOT IN ("`.
fn finish_participant_filter(
    table: &str,
    artist_id: &str,
    roles: &[&str],
    negate: bool,
) -> (String, Vec<Value>) {
    let (subquery, args) = if roles.is_empty() {
        participant_ids_subquery(table, artist_id)
    } else {
        with_participant_roles(table, artist_id, roles)
    };
    let op = if negate { " NOT IN (" } else { " IN (" };
    let id_column = participant_id_column(table);
    (format!("{id_column}{op}{subquery})"), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "album";
    const ARTIST: &str = "art-1";

    #[test]
    fn participant_id_filter_renders_positive_semi_join() {
        // Arrange: ParticipantIDFilter with no roles (sql_participations.go:21-24).
        // Act:
        let (sql, args) = participant_id_filter(TABLE, ARTIST, &[]);
        // Assert:
        assert_eq!(
            sql,
            "album.id IN (SELECT album_id FROM album_artists WHERE artist_id = ?)"
        );
        assert_eq!(args, vec![Value::Text(ARTIST.to_string())]);
    }

    #[test]
    fn participant_id_filter_with_roles_renders_role_in() {
        // Arrange: ParticipantIDFilter with roles (sql_participations.go:29-31).
        // Act:
        let (sql, args) = participant_id_filter(TABLE, ARTIST, &["albumartist", "composer"]);
        // Assert:
        assert_eq!(
            sql,
            "album.id IN (SELECT album_id FROM album_artists \
             WHERE artist_id = ? AND role IN (?, ?))"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(ARTIST.to_string()),
                Value::Text("albumartist".to_string()),
                Value::Text("composer".to_string()),
            ]
        );
    }

    #[test]
    fn not_participant_id_filter_renders_negated_semi_join() {
        // Arrange: NotParticipantIDFilter with roles (sql_participations.go:26-28).
        // Act:
        let (sql, args) = not_participant_id_filter("media_file", ARTIST, &["lyricist"]);
        // Assert:
        assert_eq!(
            sql,
            "media_file.id NOT IN (SELECT media_file_id FROM media_file_artists \
             WHERE artist_id = ? AND role IN (?))"
        );
        assert_eq!(
            args,
            vec![
                Value::Text(ARTIST.to_string()),
                Value::Text("lyricist".to_string()),
            ]
        );
    }

    #[test]
    fn with_participant_roles_panics_on_empty_roles() {
        // Arrange: the guard Go applies before reaching the role Eq (:29).
        let result = std::panic::catch_unwind(|| with_participant_roles(TABLE, ARTIST, &[]));
        // Assert:
        assert!(result.is_err());
    }
}
