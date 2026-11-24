# SC-04 Go-truth brief — multi-track album aggregation + semantic equality

Captured verbatim from Paseo agent `860a0de3-f2df-48f4-a21e-7be34138a6e2`
(host `srv_QsD9e1TgIsNq`, workspace `ws_33y2ixfy6y65xn9pypqnzo75ba`, worktree
`/Users/nathanielheitsch/.paseo/worktrees/0xo97qr3/wizardly-elephant`, branch
`task/0xo97qr3/wizardly-elephant`, base `ec6e4a08`). The agent archived itself;
this file is the durable capture. Raw evidence also at
`/Users/nathanielheitsch/.pi/agent/sessions/--Users-nathanielheitsch-.paseo-worktrees-0xo97qr3-wizardly-elephant--/pi-fold-evidence/pi-fold-mcp/sha256/3f/3f11c3375d752b0d021bd3e60c82f4496cf1b1972799fc3863816915270a7b2b.json`
(sha256 `3f11c3375d752b0d021bd3e60c82f4496cf1b1972799fc3863816915270a7b2b`).

## 1. VERDICT: READY-WITH-CAVEATS

Aggregation and both equality rules are fully established from Go source **and**
executed against the real Go code (temporary `package main` harness outside the
repo, plus the tracked Go suites). Nothing blocks implementation. Caveats:

1. Retiring `ProjectionError::MultiTrackGroup` changes a public enum matched by
   **landed** SC-05 code/tests (`nd-scanner/src/phase1.rs:333`,
   `tests/phase1_faults.rs:827-861`) — parent approval needed for that edit.
2. No Rust helper exists for Go's **tag-ID-based** frequency rule, so
   `album_tags` needs a real implementation, not a generalization of the
   singleton arm.
3. Go's album equality **ignores** Tags/Participants while media equality
   **includes** them and includes the derived `genre` — both are executed
   surprises that a naive port gets wrong.

Executed evidence (fresh, that session):
- `go test ./model/ -count=1` → **387/387 specs PASS**;
  `-ginkgo.focus="ToAlbum"` → 32 PASS; `-ginkgo.focus="GroupByFrequency|Hash"` → 12 PASS.
- `go test ./scanner/ -ginkgo.focus="filterUnmodified"` → **3 PASS** (unchanged / changed / empty).
- `cargo test --locked -p nd-scanner --test projection_singleton --test mapping_bridge` → **4 PASS / 16 PASS**.
- Go probe harness `/tmp/sc04check` (read-only on the repo; `replace … => <worktree>`)
  — every rule marked **E** below is a printed result from that harness.

## 2. EXACT CONTRACT (Go truth)

### A. `MediaFiles.ToAlbum` — `model/mediafile.go:320-409`

1. Empty group → `Album{}` zero value, **nil** `Discs`, nil `FolderIDs`
   (`:321-323`). **E**: `MediaFiles{}.ToAlbum().Equals(Album{}) == true`.
2. Seed `SongCount = len(mfs)`, `Tags: make(Tags)`,
   `Participants: make(Participants)`, `Discs{1:""}` (`:324`).
3. Sort the group by **`Path`** only (`slices.SortFunc`, `:327`).
   **E: this mutates the caller's slice in place** (`before=[Z A M] after=[A M Z]`).
   Subplan item 4 requires sorting a **copy**.
4. "Assume all the same" block — plain assignment per track, so **last track in
   path order wins**: `ID/LibraryID/Name/AlbumArtist/AlbumArtistID/SortAlbumName/
   SortAlbumArtistName/OrderAlbumName/OrderAlbumArtistName/MbzAlbumArtistID/
   MbzAlbumType/MbzAlbumComment/CatalogNum` (`:346-358`).
   **E**: `{a:Name="FromA",…}` + `{b:Name="FromB"}` → `Name="FromB"`,
   `AlbumArtist=""`, `CatalogNum=""` (empty last-track value overwrites a
   non-empty earlier one).
5. `Compilation` = **OR** (`:359`). **E true**.
6. `Duration += m.Duration` — **float32 accumulation in path order** (`:362`);
   `Size += m.Size` int64 (`:363`). **E**: 100.2+200.2+150.6 → `451.0`, size
   `4072` (matches `model/mediafile_test.go:101-110`).
7. Collector slices per track: years, dates, originalYears/Dates, releaseDates,
   `RGAlbumGain/Peak` pointers, comments, mbzAlbumIds, mbzReleaseGroupIds
   (`:364-373`).
8. `Discs`: seeded `{1:""}`; `if m.DiscNumber > 0 { Discs.Add(number, subtitle) }`
   (`:374-376`), so same disc → **last path-sorted subtitle wins**;
   `DiscNumber <= 0` adds nothing. **E**: `{1:"First"},{1:"Second"}` →
   `{1:"Second"}`; `{0:"ignored"},{-1}` → `{1:""}`.
9. `tags = append(tags, m.Tags.FlattenAll()...)` (`:377`, `model/tag.go:111-119`)
   then `a.SetTags(tags)` (`:395`).
10. `a.Participants.Merge(m.Participants)` (`:378`; `model/participants.go:129-133`,
    `add` dedups by `ID+SubRole`, **first occurrence wins**, `:135-147`).
    **E**: same id+subrole different name → keeps `First`; same id different
    subrole → both kept.
11. Artwork: `firstArtPath(current, currentDisc, m)` (`:381`, `:502-520`): skip if
    no art; win if `currentDisc==0` **or** (`m.DiscNumber < currentDisc && > 0`);
    tie on equal disc → lower `Path`. **E**: lowest disc wins over path
    (`mediafile_test.go:421-445`); equal disc → lowest path; **quirk** — a
    `DiscNumber==0` art track resets state to disc 0, so any **later** art track
    wins unconditionally: `{a:disc0},{b:disc0},{c:disc0}` →
    `EmbedArtPath="c"` (last by path); `{a:disc1 no art},{b:disc0 art},{c:disc2 art}` → `"c"`.
12. `ExplicitStatus`: `e` beats `c` regardless of order; `c` only if current isn't
    `e`; empty is a no-op (`:383-387`). **E**: both orderings → `"e"`.
13. `UpdatedAt = newer(...)` newest (`:389`), `CreatedAt = older(...)` oldest
    ignoring zeros (`:390`, `:469-480`), `Missing = Missing && m.Missing` starting
    `true` (`:341`, `:391`). **E**: `all-zero BirthTime → zero`; `Missing` stays
    false for fresh tracks.
14. `EmbedArtPath` (`:394`); `SetTags` (`:395`); `FolderIDs = slice.Unique(Map(FolderID))`
    first-seen unique, **post-sort order** (`:396`, `utils/slice/slice.go:120-129`).
    **E** `[F1 F2]` (path-sorted).
15. `Date/OriginalDate/ReleaseDate/Comment = allOrNothing(...)` (`:397-402`,
    `:412-421`): unique; **0 items → ""**; **>1 distinct → ""**; else the single
    value **verbatim, malformed strings included**.
    **E**: `["not-a-date","not-a-date"] → "not-a-date"`; `["", "1985"] → ""`;
    comments likewise.
16. `minMax` years (`:400-401`, `:423-433`): max = plain max; min ignores zeros
    **unless all zero**. **E**: `{0,0,1999} → 1999/1999`; `{2001,0,1999} → 1999/2001`;
    `{-5,1999} → -5/1999`. **Divergence in the existing Rust arm**:
    `projection.rs:425-437` starts `mx=0`, so an all-negative group yields
    `(-5, 0)` where Go yields `(-5,-5)` (Go seeds `mn=mx=items[0]`).
17. `MbzAlbumID/MbzReleaseGroupID = slice.MostFrequent` — **skips the zero value**,
    ties go to the first value reaching the running max
    (`utils/slice/slice.go:83-105`). **E**: `{id2,id1} → "id2"`; `{z,"",z} → "z"`;
    all empty → `""`.
18. `RGAlbumGain/Peak = mostFrequentPtr` — nils skipped, counts by **dereferenced
    value**, genuine `0.0` is a candidate, all-nil → nil (`:405-406`, `:439-461`).
    **E**: `-8×2 vs -5×1 → -8`; `0.0×2 → 0.0`; all nil → nil;
    `1.0×1 vs 0.0×2 → 0.0`.
19. `fixAlbumArtist` (`:407`, `:484-498`): non-compilation with empty
    `AlbumArtistID` → `Participants.First(RoleArtist)` (zero Artist if role absent);
    compilation with **>1 distinct** album-artist ids → `Various Artists` /
    `63sqASlAfjbGMuLP4JhnZU`. **E** both arms, incl. `{X1,X2} → "Various Artists"`.

### B. Album tag aggregation — `model/album.go:115-122` + `model/tag.go:26-57`

`SetTags` = `TagList.GroupByFrequency()` then drop names not in
`AlbumLevelTags()` (`album.go:104-113`, `resources/mappings.yaml` `album: true`).
Frequency grouping keys on the **tag ID** =
`id.NewTagID(lower(name), lower(value))` (`model/tag.go:63-75`,
`model/id/id.go:46-48`), so **case-only differences collapse into one ID and
their frequencies combine**; the display value is the **last** occurrence's
(`tag.go:36`). Value order per name = `freq desc, then first-seen index`
(`tag.go:48-53`). **E**: `{a:[synthwave]},{b:[Synthwave]}` → `genre=[Synthwave]`;
`{a:Punk},{b:Rock},{c:Alternative,Rock}` → `genre=[Rock, Punk, Alternative]`;
same-track duplicate collapses; ties keep first-seen.

### C. Album semantic equality — `model/album.go:87-100`

`hashstructure.Hash` with `IgnoreZeroValue:true, ZeroNil:true`, after flooring
`Duration` to `float32(math.Floor(float64(...)))` on both sides. Included field
set = everything in `album.go:19-72` **except** `hash:"ignore"`: `Annotations`,
`ItemImage`, `LibraryPath`, `LibraryName`, `Genre`, `Genres`, `Tags`,
`Participants`, `Description`, `Small/Medium/LargeImageUrl`, `ExternalUrl`,
`ExternalInfoUpdatedAt`, `ImportedAt` (`:16-22`, `:57-70`); `FolderIDs` is
`hash:"set"` (`:51`).

Executed field-by-field verdicts (`Equals` after mutating one side):
- **differs** (must be reported changed): `ID`, `LibraryID`, `EmbedArtPath`,
  `SongCount`, `Size`, `Missing`, `CreatedAt`, `UpdatedAt`, `Comment`, `Date`,
  `explicit_status`, `RGAlbumGain` (any nil↔Some, and `0.0 ↔ nil`), `Discs`
  (nil vs `{1:""}`, `{}` vs `{1:""}`, any subtitle change), `FolderIDs`
  (nil vs non-empty, `[]` vs non-empty, drop-one), `Duration ±1.0`.
- **ignored** (must be reported unchanged): `Genre`/`Genres`, `Tags`,
  `Participants`, all five external-metadata fields, `ImportedAt`,
  `LibraryPath/Name`, annotations (`Starred`), `ItemImage`, and `Duration ±0.4` (floor).
- `FolderIDs` set semantics: order-insensitive **and** duplicate-insensitive
  (`[F1,F2] ↔ [F1,F2,F2]` equal); **nil ≠ `[]`** on both `Discs` and `FolderIDs`.
- Pointer/zero asymmetry: `nil` is skipped by `IgnoreZeroValue` while
  `Some(0.0)` is hashed → **not equal**
  (`hashstructure@v1.1.0/hashstructure.go:456-466`, ptr/nil arms `:230-243`).
  Executed both directions.

### D. Media exact/equivalent equality — `model/mediafile.go:231-253`

`Hash` = struct hash (`IgnoreZeroValue`, `ZeroNil`) +
`xxh3(Tags.Hash() ‖ Participants.Hash())` (`:236-241`); `Tags.Hash` = sorted
**tag IDs** joined `|` (`tag.go:127-136`, IDs lowercase name **and** value);
`Participants.Hash` = per role `"<role>:<subRole>:<id>/…"` sorted, joined `|`
(`participants.go:188-199`). `Equals` compares hashes (`:246-248`).
Ignored (`hash:"ignore"`, `mediafile.go:28-42,51,70,100-106`): annotations,
bookmarkable, ItemImage, AlbumImage, `ID`, `PID`, `LibraryID`, `LibraryPath/Name`,
`FolderID`, `Path`, `AlbumID`, `ProbeData`, `Tags`/`Participants` **as struct
fields** (they enter via the two hashes), `Missing`, `BirthTime`, `CreatedAt`,
`UpdatedAt`.

**E**: ID/PID/LibraryID/FolderID/Path/AlbumID/Missing/BirthTime/CreatedAt/
UpdatedAt/ProbeData/annotations/images → **equal**; Title, Size, HasCoverArt,
Lyrics, `explicit_status`, `BitDepth nil vs Some(0)`, `BPM nil vs Some(0)`,
`RGTrackGain nil vs Some(-7.5)`, tags value added, participants id/subrole
changed → **unequal**; tags value **reordering within a name → unequal** (order
is hashed); tags **name/value case** differences → equal only where the derived
IDs match (`{genre:[Rock,Pop]}` vs `{GENRE:[pop,Rock]}` → **equal**);
`Tags{}` vs nil → equal, `{"genre":[]}` vs nil → **unequal**; media `Duration`
has **no floor** (`100.2000001` equal, `100.20001` unequal); participants equal
under role-map/insert reordering, unequal when `subRole` changes; artist **name**
change with same ids → **equal** (only `subRole:id` is hashed).

`IsEquivalent` = `utils.BaseName(Path)` equality (`:250-253`, `utils/files.go:19-22`).
**E**: `a/b.mp3 ↔ x/y/b.flac` true; `a/b.mp3 ↔ a/b.mp4` true;
`a/b.mp3 ↔ a/b-extra.mp3` false; `b ↔ b.mp3` true; `"" ↔ "x"` false;
`a/b/ ↔ a/b` true.

Note: media equality **includes** `genre`, which is derived at read time by
`dbMediaFile.PostScan` (`persistence/mediafile_repository.go:49-66`) — compare
DB-read rows on both sides, never a fresh mapping DTO.

### E. Consumers

`scanner/phase_3_refresh_albums.go:78-99` (empty → skip;
`album.Equals(mfs.ToAlbum())` → skip; else refresh; tests at `:73,:84,:95`),
`scanner/phase_2_missing_tracks.go:131,135,155,241,247,262,268`,
`scanner/phase_1_folders.go:328-337` (`slice.Group` by `AlbumID`, map order
nondeterministic).

## 3. REUSE POINTS (`nd-scanner`, current working tree)

`projection.rs`: `DEFAULT_DISC:66` · `ProjectionError::MultiTrackGroup:71-82` ·
`all_names:94` · `merge_credits:104` · `all_artists:125` · `artist_drafts:151` ·
`album_draft_from_track:192-337` (per-arm Go cites already inline; the field
inventory SC-04 keeps) · `album_drafts:352-385` — grouping `BTreeMap` `:359-365`,
**refusal seam `:369-374`**, singleton call `:375-383` · `participants_of:393` ·
`album_tags:405-420` (singleton degeneration at `:399-403`) ·
`min_max_year:425-437` · `most_frequent:442-459` · `fix_album_artist:464-495` ·
`track_is_missing:503-505`.

`mapping_bridge.rs`: `TrackStat:216` (`from_storage_stat:241`,
`to_mapping_file_stat:257`) · `as_i32_checked:316` · `bridge_media_file:344` ·
`tags_from_mapping:506` · `participants_from_credits:525` ·
`named_participants:550` · `all_names:569` · `effective_birth_time:591`.

Supporting: `nd_utils::slice::{unique:134, most_frequent:82, group:22}` ·
`nd_utils::files::base_name:22` · `nd_id::new_tag_id:148` ·
`nd_id::Participants::hash:246` (Go `Participants.Hash` port, `[u8;8]`) ·
`nd_db::sql_tags::{DbTags:50, marshal_tags, tags_to_genres:173}` ·
`nd_db::hydrate::row_to_album:2414` + `AlbumRow::into_record:968` (read-side
hydrated `AlbumRecord` for phase-3 comparison) ·
`nd_db::album_repo::AlbumRecord:512`.

**Missing**: no Rust port of `Tags.Hash` / `TagList.GroupByFrequency` (grep for
`group_by_frequency` → 0 hits outside docs). No `hashstructure` equivalent
anywhere in `rust/`.

**Exact seam**: `projection.rs:369-374` — replace the `group.len() > 1` refusal
with a call into `aggregate::album_draft(...)`, keeping the singleton as the
`len==1` specialization. `phase1.rs:1625-1647` supplies `stat_for`/`birth_for`
per track (stat-keyed birth lookup noted as weak at `:1637-1640`).

## 4. PROPOSED FILE PLAN (shape only)

- `nd-scanner/src/aggregate.rs` — pure, no I/O.
  `pub fn album_draft(tracks: &[&MediaFile], facts: impl Fn(&MediaFile) -> TrackFacts,
  mappings: &Mappings, imported_at: DateTime<Utc>) -> Result<AlbumRecord, AggregateError>`
  where `TrackFacts { stat: TrackStat, birth: Option<DateTime<Utc>> }` (keeps one
  resolved birth per track). Private helpers mirroring Go one-for-one:
  `sort_by_path_copy`, `all_or_nothing`, `min_max_year` (exact Go arms incl.
  negative min), `most_frequent` (zero-skipping), `most_frequent_ptr` (value
  counting, `0.0` candidate), `first_art_path` (incl. the disc-0 reset quirk),
  `fix_album_artist` (reuse projection's or move it), `flatten_tag_list`,
  `group_by_frequency` (tag-ID keyed, freq→first-seen, last display value),
  `album_level_filter`. `AggregateError { Empty, SongCountOverflow, InvalidRole }`
  — empty group returns Go's `Album{}` **zero** (`Discs: None`, `folder_ids: None`),
  not an error. No mixed-album/mixed-library check is in Go; the subplan asks for
  a programmer-error — implement as a debug_assert/explicit `MixedGroup` refusal
  on `album_id`/`library_id` disagreement, documented as a deviation.
- `nd-scanner/src/compare.rs` — pure structural equality (authorized by subplan
  item 5; no hash crate): `pub fn album_equals(a: &AlbumRecord, b: &AlbumRecord) -> bool`
  (field inventory per §C, `duration` floored, `folder_ids` compared as a dedup
  set with `None != Some(vec![])`, `rg_*` compared as `Option<f64>` with
  `None != Some(0.0)`, external-metadata/genre/tags/participants excluded);
  `pub fn media_equals(a: &MediaFileRecord, b: &MediaFileRecord) -> bool`
  (Go's include set + `tag_ids_hash` + `participants_hash`, `genre` included);
  `pub fn is_equivalent(a: &MediaFileRecord, b: &MediaFileRecord) -> bool` via
  `nd_utils::files::base_name`. Document explicitly that this proves **boolean**
  equivalence on the used field domain, not Go hash bytes.
- `projection.rs` — `album_drafts` delegates every group to
  `aggregate::album_draft` (`:369-374` → call), `album_draft_from_track` becomes
  the 1-track specialization (or stays as a thin wrapper), `album_tags` delegates
  to `aggregate::group_by_frequency` + level filter, `min_max_year`/`most_frequent`
  move to `aggregate`. `lib.rs` gains `pub mod aggregate; pub mod compare;`.

## 5. TEST PLAN (must be executed, not described)

`tests/aggregate.rs`:
- `aggregate_zero_one_many_path_sorted` — empty → all-zero record with
  `discs: None`/`folder_ids: None`; one → passthrough; three-track Go vector →
  `song_count 3`, `duration 451.0`, `size 4072`, `min/max 1985/1986`, `date ""`,
  `updated_at` newest, `created_at` oldest-ignoring-zeros.
- `permuting_input_does_not_change_the_aggregate` — non-vacuity: shuffle the
  input vec, assert byte-identical record, and assert the caller's slice is
  **not** reordered.
- `first_track_only_aggregate_fails_count_and_sums` — assert
  `song_count != 1` and `duration != first.duration`.
- `spanning_folders_discs_missing_compilation` — folder_ids unique in path order,
  `{1:"Second"}` subtitle-overwrite, `DiscNumber<=0` no-op, compilation OR.
- `art_lowest_disc_then_path` + `art_disc_zero_resets_the_state`.
- `date_comment_all_or_nothing` (`["2024","2024",""] → ""`; malformed-but-equal
  → verbatim), `min_max_year_zero_and_negative_edges`
  (`{0,0,1999}→1999/1999`, `{2001,0,1999}→1999/2001`, `{-5,1999}→-5/-5`, `{0,0}→0/0`).
- `mbz_and_rg_most_frequent` (tie→first-reached-max; `0.0` beats nil; genuine
  `0.0×2` beats `1.0×1`).
- `album_tags_frequency_and_case_folding` (Go vector →
  `genre=[Rock,Punk,Alternative]`; case-folded frequency combine; non-album-level
  names dropped).
- `fix_album_artist_both_arms`.
- `explicit_status_e_wins`.
- `song_count_overflow_u64` (u64 saturating/checked).

`tests/compare.rs`:
- `album_equals_included_fields` — table over the differs list;
  `album_equals_ignored_fields` — table over the ignored list (incl. ±0.4 floor);
  `duration_floor_boundary` (`100.9` vs `100.1` equal, `100.0` vs `99.9` not);
  `folder_ids_set_semantics` (order/dup insensitive, `None != Some(vec![])`);
  `rg_gain_none_vs_zero_not_equal`.
- `media_equals_ignored_included` tables;
  `genre_is_included_in_media_equality`;
  `tags_hash_order_sensitive_but_id_case_insensitive`;
  `participants_hash_ignores_names`;
  `empty_tags_vs_nil` (`Tags{} == nil`, `{"genre":[]} != nil`);
  `is_equivalent_basename_matrix`.
- `mixed_group_is_rejected` for the deliberate deviation.

Non-vacuity: each `*_ignored_*` table must assert the *same* mutation applied to
an **included** field flips the result, and each `*_included_*` table must pin
that the corresponding Go test/spec exists — plus a negative control where the
comparator is mutated (a `#[cfg(test)]` mutation harness is overkill; instead
assert one field per table with distinct values and confirm the opposite verdict
for its counterpart).

Integration guards:
- `projection_singleton.rs` must stay green unchanged (singleton arm is now the
  `len==1` path of the general aggregator).
- `mapping_bridge.rs` 16 tests must stay green unchanged.
- `album_tags` is the singleton degeneration of `group_by_frequency`
  (`projection.rs:399-420`); verify `album_tags` tests still pass unchanged.
- `Genre`/`Genres` remain derived (`album_draft_from_track:284-291` keeps calling
  `tags_to_genres`); equality ignores them, so phase 3 cannot churn on a
  genre-only tag edit. Also verify `album_level_tags` stays consistent with
  `mappings.yaml`'s `album: true` set.

## 6. UNKNOWNS

1. **SC-04-unknown-1 — `MultiTrackGroup` retirement touches landed SC-05 code.**
   `phase1.rs:333` documents it in `ScanPhase1Error::Projection` and
   `tests/phase1_faults.rs:827-861` asserts a 2-track group returns it. SC-04
   must delete/retire the variant, so SC-05's fault test has to change
   (`phase1_faults.rs` must assert a successful 2-track projection instead).
   That is an edit into SC-05's files — needs parent-approved sequencing, and
   `phase1.rs:40` module docs go stale. Decision needed.
2. **SC-04-unknown-2 — `AlbumRecord` field coverage.** `nd_db::album_repo::AlbumRecord`
   is the DB write DTO; Go's `Album` has fields the Rust struct may not carry
   (e.g. `Discs`/`FolderIDs` marshalling and `OrderAlbumName`/`MbzAlbumType`
   presence not verified here). Equality can only compare what exists; if a field
   is absent from `AlbumRecord`, phase-3 semantics differ from Go in ways the
   field-by-field table cannot catch. Needs a `row_to_album`/`into_record`
   inventory pass.
3. **SC-04-unknown-3 — `Tags.Hash` port does not exist.** `nd_id::Participants::hash`
   exists; there is no `Tags::hash`/`TagList` port. Media equality needs Go's
   tag-ID hash (sorted lowercase IDs joined by `|`, then xxh3 — or a comparison
   over the ID list directly, which avoids needing xxh3 at all). Decide: port
   `Tags.Hash` exactly, or prove boolean equivalence on ID sequences.
4. **SC-04-unknown-4 — float32 vs f64 duration.** Go accumulates `Duration` as
   `float32` and floors it; `MediaFile.duration` in Rust — check whether it is
   `f32` or `f64` and whether the DB column is REAL; a `f64` sum will not match
   Go's `451.0`-style float32 rounding in general (this vector happens to agree).
5. **SC-04-unknown-5 — birth-time keying.** `phase1.rs:1637-1640` resolves birth
   time keyed by *stat*, which the module docs already call weak; with
   multi-track groups two tracks can share a stat key, so `CreatedAt` (oldest
   non-zero) may resolve to the same birth for all tracks in a group. Needs
   per-path resolution to match Go semantics.
6. **SC-04-unknown-6 — `Discs` marshalling shape.** Go `Album.Discs` is
   `map[int]string` always seeded with `{1:""}`; the Rust `AlbumRecord`/
   `AlbumRow` representation and whether `None` vs `Some(empty)` survive the DB
   round-trip were not verified here.
7. **SC-04-unknown-7 — `filterUnmodified` (phase 2) media comparison source.**
   Go compares DB-read `MediaFile`s (with `PostScan`-derived `genre`), not
   freshly-mapped DTOs. Rust SC-05 currently only *refuses* multi-track groups,
   so there is no landed read-side comparison. The correct Rust read-side
   counterpart (`row_to_mediafile` with derived `genre` via tags) is not yet
   implemented; SC-04's comparator must be fed DB-read rows or it will disagree
   with Go on genre.

## 7. Validation commands the brief actually ran

- `go test ./model/ -count=1` → 387 PASS
- `go test ./model/ -ginkgo.focus="ToAlbum"` → 32 PASS
- `go test ./model/ -ginkgo.focus="GroupByFrequency|Hash"` → 12 PASS
- `go test ./scanner/ -ginkgo.focus="filterUnmodified"` → 3 PASS
- `cargo test --locked -p nd-scanner --test projection_singleton --test mapping_bridge` → 4 PASS, 16 PASS
