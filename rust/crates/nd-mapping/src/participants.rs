//! Port of `model/metadata/map_participants.go` — role/participant assignment
//! from cleaned tags.
//!
//! Go's participant construction is order-sensitive: the plural artist tag wins
//! outright (`getArtistValues`, `:201-216`), splitting applies only to a single
//! value (`:179-198`), sort names and MBIDs pair positionally with names
//! (`buildArtists`, `:156-174`), and performer MBIDs are consumed in order per
//! sub-role (`:115-121`).
//!
//! [`Credit`] here is the port of `model.Artist` (`model/artist.go:14-24`, the
//! fields Go embeds into `model.Participant`, `model/participants.go:78-81`).
//! `nd_id::Participant` (#02) models only `id`/`name`/`sort`/`sub_role`, so the
//! `OrderArtistName` and `MbzArtistID` fields the mapping produces live in this
//! crate until #02/#06 widen the shared type.

use nd_id::{Participants, Role, ROLE_ALBUM_ARTIST, ROLE_ARTIST, ROLE_PERFORMER};
use nd_utils::str::sanitize_field_for_sorting_no_article;

use crate::metadata::Metadata;
use crate::tag_names::*;
use crate::title_case::title_case;

/// One credited artist plus an optional sub-role
/// (Go `model.Participant` = embedded `model.Artist` + `SubRole`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Credit {
    /// Artist id: `artistID(name)` (`model/metadata/persistent_ids.go:124-127`).
    pub id: String,
    /// Display name (Go `Artist.Name`).
    pub name: String,
    /// Sort name (Go `Artist.SortArtistName`).
    pub sort_artist_name: String,
    /// Article-stripped, lowercased, accent-cleared sort key
    /// (Go `Artist.OrderArtistName`).
    pub order_artist_name: String,
    /// MusicBrainz artist id (Go `Artist.MbzArtistID`).
    pub mbz_artist_id: String,
    /// Sub-role qualifier, rendered as `Name (SubRole)`
    /// (Go `Participant.SubRole`).
    pub sub_role: String,
}

/// Role → credits, in insertion order per role with Go's dedup rule
/// (`Participants.add`, `model/participants.go:135-147`: dedup by
/// `id + subRole`, first occurrence wins).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleCredits {
    inner: std::collections::BTreeMap<Role, Vec<Credit>>,
}

impl RoleCredits {
    /// `Participants.Add` (`model/participants.go:97-102`).
    pub fn add(&mut self, role: Role, credits: impl IntoIterator<Item = Credit>) {
        self.add_inner(
            role,
            credits.into_iter().map(|c| Credit {
                sub_role: String::new(),
                ..c
            }),
        );
    }

    /// `Participants.AddWithSubRole` (`model/participants.go:105-110`).
    pub fn add_with_sub_role(
        &mut self,
        role: Role,
        sub_role: &str,
        credits: impl IntoIterator<Item = Credit>,
    ) {
        self.add_inner(
            role,
            credits.into_iter().map(|c| Credit {
                sub_role: sub_role.to_string(),
                ..c
            }),
        );
    }

    fn add_inner(&mut self, role: Role, credits: impl Iterator<Item = Credit>) {
        let list = self.inner.entry(role).or_default();
        let mut seen: std::collections::HashSet<String> =
            list.iter().map(|c| c.id.clone() + &c.sub_role).collect();
        for c in credits {
            let key = c.id.clone() + &c.sub_role;
            if seen.insert(key) {
                list.push(c);
            }
        }
    }

    /// `Participants.First` (`model/participants.go:121-126`): the zero-value
    /// artist when the role is absent, exactly like Go's empty `Artist{}`.
    #[must_use]
    pub fn first(&self, role: Role) -> Option<&Credit> {
        self.inner.get(&role).and_then(|l| l.first())
    }

    /// The credits for a role.
    #[must_use]
    pub fn get(&self, role: Role) -> Option<&Vec<Credit>> {
        self.inner.get(&role)
    }

    /// Mutable access to one role's credits (used by tests that prove the
    /// oracle assertions are load-bearing).
    #[must_use]
    pub fn get_mut(&mut self, role: Role) -> Option<&mut Vec<Credit>> {
        self.inner.get_mut(&role)
    }

    /// Iterates role → credits deterministically.
    pub fn iter(&self) -> impl Iterator<Item = (Role, &Vec<Credit>)> {
        self.inner.iter().map(|(r, v)| (*r, v))
    }

    /// Mutable access to each role's credits.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Vec<Credit>> {
        self.inner.values_mut()
    }

    /// Number of roles present (Go `Expect(participants).To(HaveLen(2))` in
    /// `map_participants_test.go`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether no role is present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// `Participants.Sort` (`model/participants.go:112-118`): sort each role's
    /// credits by name. The scanner sorts participants before persisting.
    pub fn sort(&mut self) {
        for list in self.inner.values_mut() {
            list.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    /// `Participants.AllIDs` (`model/participants.go:169-172`) via
    /// `AllArtists` (`:150-166`): unique by id, stably sorted by id.
    #[must_use]
    pub fn all_ids(&self) -> Vec<&str> {
        let mut all: Vec<&Credit> = self.inner.values().flatten().collect();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        all.dedup_by(|a, b| a.id == b.id);
        all.into_iter().map(|c| c.id.as_str()).collect()
    }

    /// Rebuilds an `nd_id::Participants` for consumers that only need the
    /// (#02) subset of fields that type models.
    #[must_use]
    pub fn to_nd_participants(&self) -> Participants {
        let mut out = Participants::default();
        for (role, list) in &self.inner {
            for c in list {
                out.add_with_sub_role(
                    *role,
                    &c.sub_role,
                    [nd_id::Participant {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        sort_artist_name: c.sort_artist_name.clone(),
                        sub_role: String::new(),
                    }],
                );
            }
        }
        out
    }
}

/// One role's name/sort/MBID tag triple (Go `roleTags`,
/// `model/metadata/map_participants.go:15-19`).
#[derive(Debug, Clone, Copy)]
pub struct RoleTags {
    /// The role (`model/metadata/map_participants.go:22-32`).
    pub role: Role,
    /// Name tag.
    pub name: &'static str,
    /// Sort tag; empty when Go has none for that role.
    pub sort: &'static str,
    /// MBID tag.
    pub mbid: &'static str,
}

/// `roleMappings` (`model/metadata/map_participants.go:21-32`). Go iterates a
/// map; order is irrelevant because each role writes its own list.
pub const ROLE_MAPPINGS: &[RoleTags] = &[
    RoleTags {
        role: nd_id::ROLE_COMPOSER,
        name: TAG_COMPOSER.as_str(),
        sort: TAG_COMPOSER_SORT.as_str(),
        mbid: TAG_MUSIC_BRAINZ_COMPOSER_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_LYRICIST,
        name: TAG_LYRICIST.as_str(),
        sort: TAG_LYRICIST_SORT.as_str(),
        mbid: TAG_MUSIC_BRAINZ_LYRICIST_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_CONDUCTOR,
        name: TAG_CONDUCTOR.as_str(),
        sort: "",
        mbid: TAG_MUSIC_BRAINZ_CONDUCTOR_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_ARRANGER,
        name: TAG_ARRANGER.as_str(),
        sort: "",
        mbid: TAG_MUSIC_BRAINZ_ARRANGER_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_DIRECTOR,
        name: TAG_DIRECTOR.as_str(),
        sort: "",
        mbid: TAG_MUSIC_BRAINZ_DIRECTOR_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_PRODUCER,
        name: TAG_PRODUCER.as_str(),
        sort: "",
        mbid: TAG_MUSIC_BRAINZ_PRODUCER_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_ENGINEER,
        name: TAG_ENGINEER.as_str(),
        sort: "",
        mbid: TAG_MUSIC_BRAINZ_ENGINEER_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_MIXER,
        name: TAG_MIXER.as_str(),
        sort: "",
        mbid: TAG_MUSIC_BRAINZ_MIXER_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_REMIXER,
        name: TAG_REMIXER.as_str(),
        sort: "",
        mbid: TAG_MUSIC_BRAINZ_REMIXER_ID.as_str(),
    },
    RoleTags {
        role: nd_id::ROLE_DJMIXER,
        name: TAG_DJ_MIXER.as_str(),
        sort: "",
        mbid: TAG_MUSIC_BRAINZ_DJ_MIXER_ID.as_str(),
    },
];

/// `md.mapParticipants()` (`model/metadata/map_participants.go:34-76`).
#[must_use]
pub fn map_participants(md: &Metadata) -> RoleCredits {
    let mut participants = RoleCredits::default();

    // Track artists (`:37-43`).
    let artists = md.parse_artists(
        TAG_TRACK_ARTIST.as_str(),
        TAG_TRACK_ARTISTS.as_str(),
        TAG_TRACK_ARTIST_SORT.as_str(),
        TAG_TRACK_ARTISTS_SORT.as_str(),
        TAG_MUSIC_BRAINZ_ARTIST_ID.as_str(),
    );
    participants.add(ROLE_ARTIST, artists.iter().cloned());

    // Album artists (`:45-58`).
    let mut album_artists = md.parse_artists(
        TAG_ALBUM_ARTIST.as_str(),
        TAG_ALBUM_ARTISTS.as_str(),
        TAG_ALBUM_ARTIST_SORT.as_str(),
        TAG_ALBUM_ARTISTS_SORT.as_str(),
        TAG_MUSIC_BRAINZ_ALBUM_ARTIST_ID.as_str(),
    );
    if album_artists.len() == 1 && album_artists[0].name == nd_id::UNKNOWN_ARTIST {
        if md.bool(TAG_COMPILATION.as_str()) {
            album_artists = md.build_artists(
                vec![nd_id::VARIOUS_ARTISTS.to_string()],
                &[],
                &[nd_id::VARIOUS_ARTISTS_MBZ_ID.to_string()],
            );
        } else {
            album_artists = artists.clone();
        }
    }
    participants.add(ROLE_ALBUM_ARTIST, album_artists.iter().cloned());

    // All other roles (`:60-69`).
    for info in ROLE_MAPPINGS {
        let names = md.get_role_values(info.name);
        if names.is_empty() {
            continue;
        }
        let sorts = md.strings(info.sort);
        let mbids = md.strings(info.mbid);
        let artists = md.build_artists(names, &sorts, &mbids);
        participants.add(info.role, artists.iter().cloned());
    }

    // Performers by sub-role, then MBID sync (`:71-73`).
    let roles_mbid_map = md.build_role_mbid_maps();
    md.process_performers(&mut participants, &roles_mbid_map);
    md.sync_missing_mbz_ids(&mut participants);

    participants
}

impl Metadata {
    /// `parseArtists` (`map_participants.go:143-154`): falls back to a single
    /// `[Unknown Artist]` when no name tag is present.
    pub(crate) fn parse_artists(
        &self,
        name: &str,
        names: &str,
        sort: &str,
        sorts: &str,
        mbid: &str,
    ) -> Vec<Credit> {
        let mut name_values = self.get_artist_values(name, names);
        if name_values.is_empty() {
            name_values = vec![nd_id::UNKNOWN_ARTIST.to_string()];
        }
        let sort_values = self.get_artist_values(sort, sorts);
        let mbids = self.strings(mbid);
        self.build_artists(name_values, &sort_values, &mbids)
    }

    /// `buildArtists` (`map_participants.go:156-174`): positional sort/MBID
    /// pairing; extra names simply get no sort/MBID.
    pub(crate) fn build_artists(
        &self,
        names: Vec<String>,
        sorts: &[String],
        mbids: &[String],
    ) -> Vec<Credit> {
        let articles = self.options().ignored_articles.as_str();
        names
            .into_iter()
            .enumerate()
            .map(|(i, name)| Credit {
                order_artist_name: sanitize_field_for_sorting_no_article(&name, articles),
                sort_artist_name: sorts.get(i).cloned().unwrap_or_default(),
                mbz_artist_id: mbids.get(i).cloned().unwrap_or_default(),
                id: self.artist_id(&name),
                name,
                sub_role: String::new(),
            })
            .collect()
    }

    /// `getArtistValues` (`map_participants.go:201-216`): the plural tag wins
    /// outright; a lone single-tag value is split, but several values of a
    /// single-valued tag are returned untouched.
    pub(crate) fn get_artist_values(&self, single: &str, multi: &str) -> Vec<String> {
        let v_multi = self.strings(multi);
        if !v_multi.is_empty() {
            return v_multi;
        }
        let v_single = self.strings(single);
        if v_single.len() != 1 {
            return v_single;
        }
        let table = self.mappings();
        // Go: `conf := TagMainMappings()[single]; if conf.Split == nil { conf = TagArtistsConf() }`.
        let conf = split_conf(table.main().get(single), table.artists());
        conf.with_participant_exceptions(single, self.exceptions())
            .split_participant_values(&v_single)
    }

    /// `getRoleValues` (`map_participants.go:187-198`): same shape, `roles:` as
    /// the fallback conf.
    pub(crate) fn get_role_values(&self, role: &str) -> Vec<String> {
        let values = self.strings(role);
        if values.is_empty() {
            return Vec::new();
        }
        let table = self.mappings();
        let conf = split_conf(table.main().get(role), table.roles());
        conf.with_participant_exceptions(role, self.exceptions())
            .split_participant_values(&values)
    }

    /// `buildRoleMbidMaps` (`map_participants.go:79-88`): sub-role → MBIDs,
    /// title-cased to match the performer keys.
    pub(crate) fn build_role_mbid_maps(&self) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for pair in self.pairs(TAG_MUSIC_BRAINZ_PERFORMER_ID.as_str()) {
            let role = title_case(pair.key());
            match out.iter_mut().find(|(k, _)| *k == role) {
                Some((_, list)) => list.push(pair.value().to_string()),
                None => out.push((role, vec![pair.value().to_string()])),
            }
        }
        out
    }

    /// `processPerformers` (`map_participants.go:90-112`).
    pub(crate) fn process_performers(
        &self,
        participants: &mut RoleCredits,
        roles_mbid_map: &[(String, Vec<String>)],
    ) {
        // Go: `model.TagRolesConf().WithParticipantExceptions(model.TagPerformer)`.
        let conf = self
            .mappings()
            .roles()
            .clone()
            .with_participant_exceptions(TAG_PERFORMER.as_str(), self.exceptions());
        let mut role_idx: Vec<(String, usize)> = Vec::new();

        for pair in self.pairs(TAG_PERFORMER.as_str()) {
            let sub_role = title_case(pair.key());
            let names = conf.split_participant_values(&[pair.value().to_string()]);
            for name in names {
                let mbid = get_performer_mbid(&sub_role, roles_mbid_map, &mut role_idx);
                participants.add_with_sub_role(
                    ROLE_PERFORMER,
                    &sub_role,
                    [Credit {
                        order_artist_name: sanitize_field_for_sorting_no_article(
                            &name,
                            self.options().ignored_articles.as_str(),
                        ),
                        id: self.artist_id(&name),
                        name,
                        mbz_artist_id: mbid,
                        ..Default::default()
                    }],
                );
            }
        }
    }

    /// `syncMissingMbzIDs` (`map_participants.go:124-141`): an artist present
    /// in the artist/album-artist roles lends its MBID to every other role
    /// (matched by name).
    pub(crate) fn sync_missing_mbz_ids(&self, participants: &mut RoleCredits) {
        let mut by_name: Vec<(String, String)> = Vec::new();
        for role in [ROLE_ARTIST, ROLE_ALBUM_ARTIST] {
            if let Some(list) = participants.get(role).cloned() {
                for c in list {
                    if !c.mbz_artist_id.is_empty() {
                        by_name.push((c.name, c.mbz_artist_id));
                    }
                }
            }
        }
        if by_name.is_empty() {
            return;
        }
        for list in participants.iter_mut() {
            for c in list.iter_mut() {
                if c.mbz_artist_id.is_empty() {
                    if let Some((_, mbid)) = by_name.iter().find(|(n, _)| *n == c.name) {
                        c.mbz_artist_id = mbid.to_string();
                    }
                }
            }
        }
    }
}

/// Go's `conf.Split == nil` check: an absent or empty split list falls back to
/// the `artists:`/`roles:` conf.
fn split_conf<'a>(
    main: Option<&'a crate::mappings::TagConf>,
    fallback: &'a crate::mappings::TagConf,
) -> crate::mappings::TagConf {
    match main {
        Some(conf) if !conf.split.is_empty() => conf.clone(),
        _ => fallback.clone(),
    }
}

/// `getPerformerMbid` (`map_participants.go:115-121`): hand out the next MBID
/// for this sub-role, empty once exhausted.
fn get_performer_mbid(
    sub_role: &str,
    roles_mbid_map: &[(String, Vec<String>)],
    role_idx: &mut Vec<(String, usize)>,
) -> String {
    let Some((_, mbids)) = roles_mbid_map.iter().find(|(k, _)| k == sub_role) else {
        return String::new();
    };
    let idx = role_idx.iter().position(|(k, _)| k == sub_role).unwrap_or({
        role_idx.push((sub_role.to_string(), 0));
        role_idx.len() - 1
    });
    let i = role_idx[idx].1;
    if i >= mbids.len() {
        return String::new();
    }
    role_idx[idx].1 += 1;
    mbids[i].clone()
}

/// `mapDisplayArtist` (`map_participants.go:225-230`).
#[must_use]
pub fn map_display_artist(md: &Metadata) -> String {
    display_name(md, TAG_TRACK_ARTIST.as_str(), TAG_TRACK_ARTISTS.as_str())
        .unwrap_or_else(|| nd_id::UNKNOWN_ARTIST.to_string())
}

/// `mapDisplayAlbumArtist` (`map_participants.go:232-242`).
#[must_use]
pub fn map_display_album_artist(md: &Metadata, participant_name: &str) -> String {
    let mut fallback = nd_id::UNKNOWN_ARTIST.to_string();
    if md.bool(TAG_COMPILATION.as_str()) {
        fallback = nd_id::VARIOUS_ARTISTS.to_string();
    }
    display_name(md, TAG_ALBUM_ARTIST.as_str(), TAG_ALBUM_ARTISTS.as_str())
        .or_else(|| {
            if participant_name.is_empty() {
                None
            } else {
                Some(participant_name.to_string())
            }
        })
        .unwrap_or(fallback)
}

/// `mapDisplayName` (`map_participants.go:218-223`): join the singular tag's
/// values with `Scanner.ArtistJoiner`, falling back to the plural tag.
fn display_name(md: &Metadata, singular: &str, plural: &str) -> Option<String> {
    let joiner = md.options().artist_joiner.as_str();
    let singular_joined = md.all().get(singular).map(|v| v.join(joiner));
    match singular_joined {
        Some(s) if !s.is_empty() => Some(s),
        _ => md
            .all()
            .get(plural)
            .map(|v| v.join(joiner))
            .filter(|s| !s.is_empty()),
    }
}
