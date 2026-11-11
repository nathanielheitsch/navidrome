//! Port of `model/metadata/map_mediafile.go` — `Info` → [`MediaFile`].
//!
//! The audio-property fields come straight from `nd-tagreader`'s `Info`. Where
//! Lofty cannot supply a value the field stays absent rather than defaulted
//! (`bit_depth: None` for 0, `size`/`birth_time`/`updated_at` `None` until #17
//! supplies storage stat data) — see the crate README for the persisted-value
//! risk assessment.

use std::collections::BTreeMap;

use nd_utils::files::base_name;
use nd_utils::str::{sanitize_field_for_sorting, sanitize_field_for_sorting_no_article};

use crate::config::PidConfig;
use crate::mdfile::MediaFile;
use crate::metadata::Metadata;
use crate::participants::{map_display_album_artist, map_display_artist, map_participants};
use crate::persistent_ids::{album_id, track_pid};
use crate::tag_names::*;

impl Metadata {
    /// `ToMediaFile` (`model/metadata/map_mediafile.go:18-115`).
    #[must_use]
    pub fn to_media_file(&self, library_id: i32, folder_id: &str, pid: PidConfig<'_>) -> MediaFile {
        let mut mf = MediaFile {
            library_id,
            folder_id: folder_id.to_string(),
            tags: self.all().clone(),
            ..Default::default()
        };

        // Title and Album (`:25-32`).
        mf.title = self.map_track_title();
        mf.album = self.map_album_name();
        mf.sort_title = self.string(TAG_TITLE_SORT.as_str());
        mf.sort_album_name = self.string(TAG_ALBUM_SORT.as_str());
        let articles = self.options().ignored_articles.as_str();
        mf.order_title = sanitize_field_for_sorting(&mf.title);
        mf.order_album_name = sanitize_field_for_sorting_no_article(&mf.album, articles);
        mf.compilation = self.bool(TAG_COMPILATION.as_str());

        // Disc and track info (`:34-47`).
        mf.track_number = self.num_and_total(TAG_TRACK_NUMBER.as_str()).0;
        mf.disc_number = self.num_and_total(TAG_DISC_NUMBER.as_str()).0;
        mf.disc_subtitle = self.string(TAG_DISC_SUBTITLE.as_str());
        mf.catalog_num = self.string(TAG_CATALOG_NUMBER.as_str());
        mf.comment = self.string(TAG_COMMENT.as_str());
        if let Some(f) = self.nullable_float(TAG_BPM.as_str()) {
            let r = f.round();
            if r > 0.0 && r <= f64::from(i32::MAX) {
                mf.bpm = Some(r as i32);
            }
        }
        mf.lyrics = self.map_lyrics();
        mf.explicit_status = self.map_explicit_status_tag();
        self.map_movement_numbers(&mut mf.tags);

        // Dates (`:49-53`).
        let (date, original_date, release_date) = self.map_dates();
        mf.original_year = date_year(&original_date);
        mf.original_date = original_date;
        mf.release_year = date_year(&release_date);
        mf.release_date = release_date;
        mf.year = date_year(&date);
        mf.date = date;

        // MBIDs (`:55-60`).
        mf.mbz_recording_id = self.string(TAG_MUSIC_BRAINZ_RECORDING_ID.as_str());
        mf.mbz_release_track_id = self.string(TAG_MUSIC_BRAINZ_TRACK_ID.as_str());
        mf.mbz_album_id = self.string(TAG_MUSIC_BRAINZ_ALBUM_ID.as_str());
        mf.mbz_release_group_id = self.string(TAG_MUSIC_BRAINZ_RELEASE_GROUP_ID.as_str());
        mf.mbz_album_type = self.string(TAG_RELEASE_TYPE.as_str());

        // ReplayGain (`:62-66`).
        mf.rg_album_peak = self.nullable_float(TAG_REPLAY_GAIN_ALBUM_PEAK.as_str());
        mf.rg_album_gain = self.map_gain(
            TAG_REPLAY_GAIN_ALBUM_GAIN.as_str(),
            TAG_R128_ALBUM_GAIN.as_str(),
        );
        mf.rg_track_peak = self.nullable_float(TAG_REPLAY_GAIN_TRACK_PEAK.as_str());
        mf.rg_track_gain = self.map_gain(
            TAG_REPLAY_GAIN_TRACK_GAIN.as_str(),
            TAG_R128_TRACK_GAIN.as_str(),
        );

        // General properties (`:68-82`).
        mf.has_cover_art = self.has_picture();
        mf.duration = self.length();
        mf.bit_rate = as_i32(self.audio_properties().bit_rate);
        mf.sample_rate = as_i32(self.audio_properties().sample_rate);
        if self.audio_properties().bit_depth > 0 {
            mf.bit_depth = Some(as_i32(self.audio_properties().bit_depth));
        }
        mf.channels = as_i32(self.audio_properties().channels);
        mf.codec = self.audio_properties().codec.clone();
        mf.path = self.file_path().to_string();
        mf.suffix = self.suffix();
        // Go reads these off `Info.FileInfo`, which storage fills
        // (`core/storage/local/local.go:90-109`). The Rust tag reader does not
        // produce them, so they stay absent for #17 to fill from storage.
        mf.size = self.size();
        mf.updated_at = self.mod_time().map(str::to_string);
        mf.birth_time = self.birth_time().map(str::to_string);

        // Participants and display names (`:84-86`).
        mf.participants = map_participants(self);
        mf.artist = map_display_artist(self);
        mf.album_artist =
            map_display_album_artist(self, participant_name(&mf, nd_id::ROLE_ALBUM_ARTIST));

        // Persistent IDs (`:88-91`).
        mf.pid = track_pid(&mf, self, pid);
        mf.album_id = album_id(&mf, self, pid.album);

        // Deprecated single-artist fields (`:93-102`).
        mf.artist_id = participant_id(&mf, nd_id::ROLE_ARTIST);
        mf.album_artist_id = participant_id(&mf, nd_id::ROLE_ALBUM_ARTIST);
        mf.order_artist_name = participant_order_name(&mf, nd_id::ROLE_ARTIST);
        mf.order_album_artist_name = participant_order_name(&mf, nd_id::ROLE_ALBUM_ARTIST);
        mf.sort_artist_name = participant_sort_name(&mf, nd_id::ROLE_ARTIST);
        mf.sort_album_artist_name = participant_sort_name(&mf, nd_id::ROLE_ALBUM_ARTIST);

        // Drop main tags that are first-class fields and not album-level, to
        // avoid redundancy in the DB (`:104-112`).
        for (tag, conf) in self.mappings().main() {
            if !conf.album {
                mf.tags.remove(tag);
            }
        }

        mf
    }

    /// `mapTrackTitle` (`persistent_ids.go:129-134`): title, else the file
    /// stem.
    #[must_use]
    pub fn map_track_title(&self) -> String {
        let title = self.string(TAG_TITLE.as_str());
        if !title.is_empty() {
            return title;
        }
        base_name(self.file_path())
    }

    /// `mapAlbumName` (`persistent_ids.go:136-141`).
    #[must_use]
    pub fn map_album_name(&self) -> String {
        let album = self.string(TAG_ALBUM.as_str());
        if album.is_empty() {
            nd_id::UNKNOWN_ALBUM.to_string()
        } else {
            album
        }
    }

    /// `mapGain` (`map_mediafile.go:121-139`): ReplayGain wins; R128 is Q7.8
    /// divided by 256 plus the 5 dB normalization offset.
    #[must_use]
    pub fn map_gain(&self, rg: &str, r128: &str) -> Option<f64> {
        if let Some(v) = self.gain(rg) {
            return Some(v);
        }
        let r128_value = self.string(r128);
        if r128_value.is_empty() {
            return None;
        }
        // Go's `strconv.Atoi` — unparseable yields nil, and out-of-range values
        // come back saturated (the error is ignored).
        let v = crate::metadata::parse_go_i64_saturating(&r128_value)?;
        Some(f64::from(v as i32) / 256.0 + 5.0)
    }

    /// `mapLyrics` (`map_mediafile.go:162-190`): every pair is parsed
    /// (`model.ParseLyrics` with an empty suffix, so the content is sniffed),
    /// empty lyrics dropped, and the list serialized as JSON — `[]` when none.
    #[must_use]
    pub fn map_lyrics(&self) -> String {
        let mut list: nd_lyrics::LyricList = Vec::new();
        for raw in self.pairs(TAG_LYRICS.as_str()) {
            let lang = raw.key();
            let text = raw.value();
            match nd_lyrics::parse_lyrics("", lang, text.as_bytes()) {
                Ok(lyrics) => list.extend(lyrics.into_iter().filter(|l| !l.is_empty())),
                Err(_) => continue,
            }
        }
        nd_lyrics::model::lyric_list_to_json(&list)
    }

    /// `mapExplicitStatusTag` (`map_mediafile.go:192-201`).
    #[must_use]
    pub fn map_explicit_status_tag(&self) -> String {
        match self.first(TAG_EXPLICIT_STATUS.as_str()).as_str() {
            "1" | "4" => "e".to_string(),
            "2" => "c".to_string(),
            _ => String::new(),
        }
    }

    /// `mapMovementNumbers` (`map_mediafile.go:141-160`): split `n/N` movement
    /// values like `tuple()` does for track/disc, but only when at least one
    /// value carries a total; the derived totals are used only when no explicit
    /// total tag exists and *every* value had one.
    pub(crate) fn map_movement_numbers(&self, tags: &mut BTreeMap<String, Vec<String>>) {
        let values = self.strings(TAG_MOVEMENT_NUMBER.as_str());
        if !values.iter().any(|v| v.contains('/')) {
            return;
        }
        let mut numbers = Vec::with_capacity(values.len());
        let mut totals = Vec::with_capacity(values.len());
        let mut all_have_totals = true;
        for v in &values {
            match v.split_once('/') {
                Some((n, t)) => {
                    numbers.push(n.to_string());
                    totals.push(t.to_string());
                }
                None => {
                    numbers.push(v.clone());
                    totals.push(String::new());
                    all_have_totals = false;
                }
            }
        }
        tags.insert(TAG_MOVEMENT_NUMBER.as_str().to_string(), numbers);
        if all_have_totals && self.strings(TAG_MOVEMENT_TOTAL.as_str()).is_empty() {
            tags.insert(TAG_MOVEMENT_TOTAL.as_str().to_string(), totals);
        }
    }

    /// `mapDates` (`map_mediafile.go:203-220`).
    #[must_use]
    pub fn map_dates(&self) -> (String, String, String) {
        let date = self.date(TAG_RECORDING_DATE.as_str());
        let original_date = self.date(TAG_ORIGINAL_DATE.as_str());
        let release_date = self.date(TAG_RELEASE_DATE.as_str());

        // Taggers historically wrote the album's release date into `date` and
        // left `releasedate` empty (`map_mediafile.go:209-216`): in that case
        // the release date becomes the `date` tag and the primary date becomes
        // the original date.
        let legacy_mappings =
            !original_date.is_empty() && release_date.is_empty() && date >= original_date;
        if legacy_mappings {
            return (original_date.clone(), original_date, date);
        }
        // `cmp.Or(date, originalDate, releaseDate)` (`:218`). Original and
        // release dates are returned exactly as tagged.
        let mut date = date;
        if date.is_empty() {
            date = original_date.clone();
        }
        if date.is_empty() {
            date = release_date.clone();
        }
        (date, original_date, release_date)
    }

    /// `Metadata.artistID` (`persistent_ids.go:124-127`).
    #[must_use]
    pub fn artist_id(&self, name: &str) -> String {
        crate::persistent_ids::artist_id(name)
    }
}

/// `Date.Year()` (`model/metadata/metadata.go:44-50`): the first four
/// characters parsed as an int, 0 when the date is empty or unparsable.
#[must_use]
pub fn date_year(date: &str) -> i32 {
    if date.is_empty() {
        return 0;
    }
    let first4: String = date.chars().take(4).collect();
    match crate::metadata::parse_go_i64(&first4) {
        Some(v) => v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        None => 0,
    }
}

/// Go's `int(props.X)` narrowing of the backend's `i64` values.
fn as_i32(v: i64) -> i32 {
    v as i32
}

fn participant_name(mf: &MediaFile, role: nd_id::Role) -> &str {
    mf.participants
        .first(role)
        .map(|c| c.name.as_str())
        .unwrap_or("")
}

fn participant_id(mf: &MediaFile, role: nd_id::Role) -> String {
    mf.participants
        .first(role)
        .map(|c| c.id.clone())
        .unwrap_or_default()
}

fn participant_order_name(mf: &MediaFile, role: nd_id::Role) -> String {
    mf.participants
        .first(role)
        .map(|c| c.order_artist_name.clone())
        .unwrap_or_default()
}

fn participant_sort_name(mf: &MediaFile, role: nd_id::Role) -> String {
    mf.participants
        .first(role)
        .map(|c| c.sort_artist_name.clone())
        .unwrap_or_default()
}
