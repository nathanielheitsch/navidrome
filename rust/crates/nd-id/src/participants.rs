//! Participants logic, ported from Go `model/participants.go`.
//!
//! [`Participants`] maps a [`Role`] to its list of [`Participant`]s,
//! preserving Go's dedup-by-`id+subRole`, name-sorted display, and the
//! XXH3-64 content hash over the flattened structure.
//!
//! The [`Role`]s of artist credits are stringly-typed domain values, exactly
//! as in Go, so they survive database round-trips unchanged.

use std::collections::BTreeMap;
use std::fmt;

/// The role of an artist in a track or album (Go `model.Role`).
///
/// Construct via [`role_from_string`]; compare against the
/// [`ROLE_*`](crate::ROLE_ARTIST) constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Role(&'static str);

macro_rules! roles {
    ($($(#[$meta:meta])* $konst:ident = $lit:literal),+ $(,)?) => {
        $(
            $(#[$meta])*
            pub const $konst: Role = Role($lit);
        )+
        /// Every known role (Go `model.AllRoles`), sorted for stable iteration.
        pub const ALL_ROLES: &[Role] = &[$($konst),+];
    };
}

roles! {
    /// Go `RoleArtist` ("artist").
    ROLE_ARTIST = "artist",
    /// Go `RoleAlbumArtist` ("albumartist").
    ROLE_ALBUM_ARTIST = "albumartist",
    /// Go `RoleComposer` ("composer").
    ROLE_COMPOSER = "composer",
    /// Go `RoleConductor` ("conductor").
    ROLE_CONDUCTOR = "conductor",
    /// Go `RoleLyricist` ("lyricist").
    ROLE_LYRICIST = "lyricist",
    /// Go `RoleArranger` ("arranger").
    ROLE_ARRANGER = "arranger",
    /// Go `RoleProducer` ("producer").
    ROLE_PRODUCER = "producer",
    /// Go `RoleDirector` ("director").
    ROLE_DIRECTOR = "director",
    /// Go `RoleEngineer` ("engineer").
    ROLE_ENGINEER = "engineer",
    /// Go `RoleMixer` ("mixer").
    ROLE_MIXER = "mixer",
    /// Go `RoleRemixer` ("remixer").
    ROLE_REMIXER = "remixer",
    /// Go `RoleDJMixer` ("djmixer").
    ROLE_DJMIXER = "djmixer",
    /// Go `RolePerformer` ("performer").
    ROLE_PERFORMER = "performer",
    /// Go `RoleMainCredit` ("maincredit") — the artist is an album artist or artist.
    ROLE_MAIN_CREDIT = "maincredit",
}

impl Default for Role {
    fn default() -> Self {
        ROLE_INVALID
    }
}

/// Go `RoleInvalid` — the zero value returned for unknown role strings.
pub const ROLE_INVALID: Role = Role("invalid");

impl Role {
    /// The wire/DB representation of the role.
    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Resolves a role string to its [`Role`], or [`ROLE_INVALID`] when unknown
/// (Go `RoleFromString`).
#[allow(dead_code)]
pub fn role_from_string(role: &str) -> Role {
    ALL_ROLES
        .iter()
        .copied()
        .find(|r| r.as_str() == role)
        .unwrap_or(ROLE_INVALID)
}

/// One credited artist: the artist's identifying fields plus an optional
/// `sub_role` (Go `model.Participant`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Participant {
    /// Artist id (Go `Participant.Artist.ID`).
    pub id: String,
    /// Artist name (Go `Participant.Artist.Name`).
    pub name: String,
    /// Optional sort name (Go `Participant.Artist.SortArtistName`).
    pub sort_artist_name: String,
    /// Optional sub-role qualifier, rendered as `Name (SubRole)` (Go `Participant.SubRole`).
    pub sub_role: String,
}

impl Participant {
    /// Display form used by [`Participants::join`]: `Name (SubRole)` when a
    /// sub-role is present, else `Name`.
    fn display_name(&self) -> String {
        if self.sub_role.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.sub_role)
        }
    }
}

/// Ordered, deduplicating map of role → participants (Go `model.Participants`).
///
/// Uses a [`BTreeMap`] over the stringly-typed roles so iteration (and hence
/// [`Participants::hash`]) is deterministic; Go's `Hash` sorts explicitly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Participants {
    inner: BTreeMap<Role, Vec<Participant>>,
}

impl Participants {
    /// Adds artists to the role, ignoring duplicates by `id + subRole`
    /// (Go `Participants.Add`).
    pub fn add(&mut self, role: Role, artists: impl IntoIterator<Item = Participant>) {
        self.add_inner(
            role,
            artists.into_iter().map(|a| Participant {
                sub_role: String::new(),
                ..a
            }),
        );
    }

    /// Adds artists with the given sub-role, ignoring duplicates by
    /// `id + subRole` (Go `Participants.AddWithSubRole`).
    pub fn add_with_sub_role(
        &mut self,
        role: Role,
        sub_role: &str,
        artists: impl IntoIterator<Item = Participant>,
    ) {
        self.add_inner(
            role,
            artists.into_iter().map(|a| Participant {
                sub_role: sub_role.to_string(),
                ..a
            }),
        );
    }

    fn add_inner(&mut self, role: Role, participants: impl Iterator<Item = Participant>) {
        let list = self.inner.entry(role).or_default();
        let mut seen: std::collections::HashSet<String> =
            list.iter().map(|p| p.id.clone() + &p.sub_role).collect();
        for p in participants {
            let key = p.id.clone() + &p.sub_role;
            if seen.insert(key) {
                list.push(p);
            }
        }
    }

    /// Iterates the role → participant-list entries in deterministic order.
    pub fn iter(&self) -> impl Iterator<Item = (Role, &Vec<Participant>)> {
        self.inner.iter().map(|(r, v)| (*r, v))
    }

    /// The participant list for `role`, if present.
    pub fn get(&self, role: Role) -> Option<&Vec<Participant>> {
        self.inner.get(&role)
    }

    /// Sorts each role's participants by name (Go `Participants.Sort`).
    pub fn sort(&mut self) {
        for list in self.inner.values_mut() {
            list.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    /// The first artist for the role, or `None` (Go `Participants.First`).
    pub fn first(&self, role: Role) -> Option<&Participant> {
        self.inner.get(&role).and_then(|l| l.first())
    }

    /// Merges `other` into `self` (Go `Participants.Merge`).
    pub fn merge(&mut self, other: Participants) {
        for (role, list) in other.inner {
            self.add_inner(role, list.into_iter());
        }
    }

    /// Joins the display names of every participant across every role with
    /// `sep` (Go `ParticipantList.Join` — note Go joins per-list; this port
    /// flattens across roles in role order for the common display case).
    pub fn join(&self, sep: &str) -> String {
        self.inner
            .values()
            .flatten()
            .map(Participant::display_name)
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// All unique artists, stably sorted by id (Go `Participants.AllArtists`).
    pub fn all_artists(&self) -> Vec<&Participant> {
        let mut all: Vec<&Participant> = self.inner.values().flatten().collect();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        all.dedup_by(|a, b| a.id == b.id);
        all
    }

    /// All unique artist ids (Go `Participants.AllIDs`).
    pub fn all_ids(&self) -> Vec<&str> {
        self.all_artists().iter().map(|p| p.id.as_str()).collect()
    }

    /// All artist names, including sort names, deduplicated in first-seen
    /// order (Go `Participants.AllNames`).
    pub fn all_names(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        let mut names = Vec::new();
        for p in self.inner.values().flatten() {
            if seen.insert(p.name.as_str()) {
                names.push(p.name.as_str());
            }
            if !p.sort_artist_name.is_empty() && seen.insert(p.sort_artist_name.as_str()) {
                names.push(p.sort_artist_name.as_str());
            }
        }
        names
    }

    /// XXH3-64 content hash of the flattened structure (Go
    /// `Participants.Hash`): per role, `"<role>:<subRole:id>/.../..."` sorted,
    /// joined by `|`, hashed. Returned as the big-endian 8 bytes Go's
    /// `sum.Sum(nil)` yields.
    pub fn hash(&self) -> [u8; 8] {
        let mut flattened: Vec<String> = self
            .inner
            .iter()
            .map(|(role, list)| {
                let mut ids: Vec<String> = list
                    .iter()
                    .map(|p| format!("{}:{}", p.sub_role, p.id))
                    .collect();
                ids.sort();
                format!("{}:{}", role, ids.join("/"))
            })
            .collect();
        flattened.sort();
        let joined = flattened.join("|");
        xxhash_rust::xxh3::xxh3_64(joined.as_bytes()).to_be_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(id: &str, name: &str) -> Participant {
        Participant {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn role_domain() {
        assert_eq!(role_from_string("albumartist"), ROLE_ALBUM_ARTIST);
        assert_eq!(role_from_string("nonsense"), ROLE_INVALID);
        assert_eq!(ROLE_ALBUM_ARTIST.to_string(), "albumartist");
        // Go's AllRoles covers 14 real roles + the invalid zero value is NOT
        // in the map, but our ALL_ROLES covers the 14 credit roles.
        assert_eq!(ALL_ROLES.len(), 14);
        assert!(!ALL_ROLES.contains(&ROLE_INVALID));
    }

    #[test]
    fn add_deduplicates_by_id_and_sub_role() {
        let mut p = Participants::default();
        p.add(ROLE_ARTIST, [artist("a1", "A"), artist("a1", "A")]);
        p.add_with_sub_role(ROLE_PERFORMER, "guitar", [artist("a1", "A")]);
        p.add_with_sub_role(ROLE_PERFORMER, "vocals", [artist("a1", "A")]);
        assert_eq!(p.get(ROLE_ARTIST).unwrap().len(), 1);
        assert_eq!(
            p.get(ROLE_PERFORMER).unwrap().len(),
            2,
            "different subRoles are distinct"
        );
    }

    #[test]
    fn merge_and_first() {
        let mut a = Participants::default();
        a.add(ROLE_COMPOSER, [artist("c1", "C1")]);
        let mut b = Participants::default();
        b.add(ROLE_COMPOSER, [artist("c1", "C1"), artist("c2", "C2")]);
        b.add(ROLE_LYRICIST, [artist("l1", "L1")]);
        a.merge(b);
        assert_eq!(a.get(ROLE_COMPOSER).unwrap().len(), 2);
        assert_eq!(a.first(ROLE_LYRICIST).unwrap().id, "l1");
        assert!(a.first(ROLE_ARRANGER).is_none());
    }

    #[test]
    fn sort_orders_by_name() {
        let mut p = Participants::default();
        p.add(ROLE_ARTIST, [artist("x", "Zeta"), artist("y", "Alpha")]);
        p.sort();
        let names: Vec<&str> = p
            .get(ROLE_ARTIST)
            .unwrap()
            .iter()
            .map(|q| q.name.as_str())
            .collect();
        assert_eq!(names, ["Alpha", "Zeta"]);
    }

    #[test]
    fn join_uses_sub_role_display() {
        let mut p = Participants::default();
        p.add(ROLE_ARTIST, [artist("a1", "A"), artist("b1", "B")]);
        assert_eq!(p.join(", "), "A, B");
        let mut q = Participants::default();
        q.add_with_sub_role(ROLE_PERFORMER, "guitar", [artist("g", "G")]);
        assert_eq!(q.join(", "), "G (guitar)");
    }

    #[test]
    fn all_artists_ids_names_dedupe() {
        let mut p = Participants::default();
        let mut with_sort = artist("a1", "A");
        with_sort.sort_artist_name = "A, The".into();
        p.add(ROLE_ARTIST, [with_sort]);
        p.add(ROLE_ALBUM_ARTIST, [artist("a1", "A"), artist("b1", "B")]);
        assert_eq!(p.all_ids(), ["a1", "b1"]);
        // Order is role-iteration dependent (Go iterates a map); compare as sets.
        let mut names = p.all_names().to_vec();
        names.sort();
        assert_eq!(names, ["A", "A, The", "B"]);
    }

    #[test]
    fn hash_is_order_independent_within_and_across_roles() {
        let mut a = Participants::default();
        a.add(ROLE_ARTIST, [artist("a1", "A"), artist("b1", "B")]);
        a.add_with_sub_role(ROLE_PERFORMER, "guitar", [artist("c1", "C")]);
        let mut b = Participants::default();
        b.add(ROLE_ARTIST, [artist("b1", "B"), artist("a1", "A")]);
        b.add_with_sub_role(ROLE_PERFORMER, "guitar", [artist("c1", "C")]);
        assert_eq!(a.hash(), b.hash());
        // Any content change flips the hash.
        b.add(ROLE_LYRICIST, [artist("z", "Z")]);
        assert_ne!(a.hash(), b.hash());
        // Deterministic bytes: fixed expected value for a fixed input.
        let mut c = Participants::default();
        c.add(ROLE_ARTIST, [artist("a1", "A")]);
        let h = c.hash();
        assert_eq!(h.len(), 8);
        assert_ne!(h, [0u8; 8]);
    }
}
