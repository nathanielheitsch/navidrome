//! Canonical tag names, ported verbatim from Go `model/tag.go:184-275`.
//!
//! The names are the keys of `resources/mappings.yaml`; Go stores tags under
//! the lowercased canonical name (`model/tag_mappings.go:259-277`), so every
//! constant here is already lowercase.

/// A canonical (lowercase) tag name (Go `model.TagName`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TagName(&'static str);

impl TagName {
    /// The wire/DB representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for TagName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

macro_rules! tag_names {
    ($($(#[$meta:meta])* $konst:ident = $lit:literal),+ $(,)?) => {
        $(
            $(#[$meta])*
            pub const $konst: TagName = TagName($lit);
        )+
    };
}

tag_names! {
    /// Go `TagAlbum` ("album").
    TAG_ALBUM = "album",
    /// Go `TagTitle` ("title").
    TAG_TITLE = "title",
    /// Go `TagTrackNumber` ("track").
    TAG_TRACK_NUMBER = "track",
    /// Go `TagDiscNumber` ("disc").
    TAG_DISC_NUMBER = "disc",
    /// Go `TagTotalTracks` ("tracktotal").
    TAG_TOTAL_TRACKS = "tracktotal",
    /// Go `TagTotalDiscs` ("disctotal").
    TAG_TOTAL_DISCS = "disctotal",
    /// Go `TagDiscSubtitle` ("discsubtitle").
    TAG_DISC_SUBTITLE = "discsubtitle",
    /// Go `TagSubtitle` ("subtitle").
    TAG_SUBTITLE = "subtitle",
    /// Go `TagGenre` ("genre").
    TAG_GENRE = "genre",
    /// Go `TagMood` ("mood").
    TAG_MOOD = "mood",
    /// Go `TagComment` (`model/tag.go:195`).
    TAG_COMMENT = "comment",
    /// Go `TagAlbumSort` ("albumsort").
    TAG_ALBUM_SORT = "albumsort",
    /// Go `TagAlbumVersion` ("albumversion").
    TAG_ALBUM_VERSION = "albumversion",
    /// Go `TagTitleSort` ("titlesort").
    TAG_TITLE_SORT = "titlesort",
    /// Go `TagCompilation` ("compilation").
    TAG_COMPILATION = "compilation",
    /// Go `TagGrouping` ("grouping").
    TAG_GROUPING = "grouping",
    /// Go `TagLyrics` ("lyrics").
    TAG_LYRICS = "lyrics",
    /// Go `TagRecordLabel` ("recordlabel").
    TAG_RECORD_LABEL = "recordlabel",
    /// Go `TagReleaseType` ("releasetype").
    TAG_RELEASE_TYPE = "releasetype",
    /// Go `TagReleaseCountry` ("releasecountry").
    TAG_RELEASE_COUNTRY = "releasecountry",
    /// Go `TagMedia` ("media").
    TAG_MEDIA = "media",
    /// Go `TagCatalogNumber` ("catalognumber").
    TAG_CATALOG_NUMBER = "catalognumber",
    /// Go `TagISRC` ("isrc").
    TAG_ISRC = "isrc",
    /// Go `TagBPM` ("bpm").
    TAG_BPM = "bpm",
    /// Go `TagExplicitStatus` ("explicitstatus").
    TAG_EXPLICIT_STATUS = "explicitstatus",
    /// Go `TagWork` ("work").
    TAG_WORK = "work",
    /// Go `TagMovementName` ("movementname").
    TAG_MOVEMENT_NAME = "movementname",
    /// Go `TagMovementNumber` ("movement").
    TAG_MOVEMENT_NUMBER = "movement",
    /// Go `TagMovementTotal` ("movementtotal").
    TAG_MOVEMENT_TOTAL = "movementtotal",

    // Dates and years (Go `model/tag.go:215-219`).

    /// Go `TagOriginalDate` ("originaldate").
    TAG_ORIGINAL_DATE = "originaldate",
    /// Go `TagReleaseDate` ("releasedate").
    TAG_RELEASE_DATE = "releasedate",
    /// Go `TagRecordingDate` ("recordingdate").
    TAG_RECORDING_DATE = "recordingdate",

    // Artists and roles (Go `model/tag.go:221-243`).

    /// Go `TagAlbumArtist` ("albumartist").
    TAG_ALBUM_ARTIST = "albumartist",
    /// Go `TagAlbumArtists` ("albumartists").
    TAG_ALBUM_ARTISTS = "albumartists",
    /// Go `TagAlbumArtistSort` ("albumartistsort").
    TAG_ALBUM_ARTIST_SORT = "albumartistsort",
    /// Go `TagAlbumArtistsSort` ("albumartistssort").
    TAG_ALBUM_ARTISTS_SORT = "albumartistssort",
    /// Go `TagTrackArtist` ("artist").
    TAG_TRACK_ARTIST = "artist",
    /// Go `TagTrackArtists` ("artists").
    TAG_TRACK_ARTISTS = "artists",
    /// Go `TagTrackArtistSort` ("artistsort").
    TAG_TRACK_ARTIST_SORT = "artistsort",
    /// Go `TagTrackArtistsSort` ("artistssort").
    TAG_TRACK_ARTISTS_SORT = "artistssort",
    /// Go `TagComposer` ("composer").
    TAG_COMPOSER = "composer",
    /// Go `TagComposerSort` ("composersort").
    TAG_COMPOSER_SORT = "composersort",
    /// Go `TagLyricist` ("lyricist").
    TAG_LYRICIST = "lyricist",
    /// Go `TagLyricistSort` ("lyricistsort").
    TAG_LYRICIST_SORT = "lyricistsort",
    /// Go `TagDirector` ("director").
    TAG_DIRECTOR = "director",
    /// Go `TagProducer` ("producer").
    TAG_PRODUCER = "producer",
    /// Go `TagEngineer` ("engineer").
    TAG_ENGINEER = "engineer",
    /// Go `TagMixer` ("mixer").
    TAG_MIXER = "mixer",
    /// Go `TagRemixer` ("remixer").
    TAG_REMIXER = "remixer",
    /// Go `TagDJMixer` ("djmixer").
    TAG_DJ_MIXER = "djmixer",
    /// Go `TagConductor` ("conductor").
    TAG_CONDUCTOR = "conductor",
    /// Go `TagArranger` ("arranger").
    TAG_ARRANGER = "arranger",
    /// Go `TagPerformer` ("performer").
    TAG_PERFORMER = "performer",

    // ReplayGain (Go `model/tag.go:245-252`).

    /// Go `TagReplayGainAlbumGain`.
    TAG_REPLAY_GAIN_ALBUM_GAIN = "replaygain_album_gain",
    /// Go `TagReplayGainAlbumPeak`.
    TAG_REPLAY_GAIN_ALBUM_PEAK = "replaygain_album_peak",
    /// Go `TagReplayGainTrackGain`.
    TAG_REPLAY_GAIN_TRACK_GAIN = "replaygain_track_gain",
    /// Go `TagReplayGainTrackPeak`.
    TAG_REPLAY_GAIN_TRACK_PEAK = "replaygain_track_peak",
    /// Go `TagR128AlbumGain`.
    TAG_R128_ALBUM_GAIN = "r128_album_gain",
    /// Go `TagR128TrackGain`.
    TAG_R128_TRACK_GAIN = "r128_track_gain",

    // MusicBrainz (Go `model/tag.go:254-274`).

    /// Go `TagMusicBrainzArtistID`.
    TAG_MUSIC_BRAINZ_ARTIST_ID = "musicbrainz_artistid",
    /// Go `TagMusicBrainzRecordingID`.
    TAG_MUSIC_BRAINZ_RECORDING_ID = "musicbrainz_recordingid",
    /// Go `TagMusicBrainzTrackID`.
    TAG_MUSIC_BRAINZ_TRACK_ID = "musicbrainz_trackid",
    /// Go `TagMusicBrainzAlbumArtistID`.
    TAG_MUSIC_BRAINZ_ALBUM_ARTIST_ID = "musicbrainz_albumartistid",
    /// Go `TagMusicBrainzAlbumID`.
    TAG_MUSIC_BRAINZ_ALBUM_ID = "musicbrainz_albumid",
    /// Go `TagMusicBrainzReleaseGroupID`.
    TAG_MUSIC_BRAINZ_RELEASE_GROUP_ID = "musicbrainz_releasegroupid",
    /// Go `TagMusicBrainzWorkID`.
    TAG_MUSIC_BRAINZ_WORK_ID = "musicbrainz_workid",
    /// Go `TagMusicBrainzComposerID`.
    TAG_MUSIC_BRAINZ_COMPOSER_ID = "musicbrainz_composerid",
    /// Go `TagMusicBrainzLyricistID`.
    TAG_MUSIC_BRAINZ_LYRICIST_ID = "musicbrainz_lyricistid",
    /// Go `TagMusicBrainzDirectorID`.
    TAG_MUSIC_BRAINZ_DIRECTOR_ID = "musicbrainz_directorid",
    /// Go `TagMusicBrainzProducerID`.
    TAG_MUSIC_BRAINZ_PRODUCER_ID = "musicbrainz_producerid",
    /// Go `TagMusicBrainzEngineerID`.
    TAG_MUSIC_BRAINZ_ENGINEER_ID = "musicbrainz_engineerid",
    /// Go `TagMusicBrainzMixerID`.
    TAG_MUSIC_BRAINZ_MIXER_ID = "musicbrainz_mixerid",
    /// Go `TagMusicBrainzRemixerID`.
    TAG_MUSIC_BRAINZ_REMIXER_ID = "musicbrainz_remixerid",
    /// Go `TagMusicBrainzDJMixerID`.
    TAG_MUSIC_BRAINZ_DJ_MIXER_ID = "musicbrainz_djmixerid",
    /// Go `TagMusicBrainzConductorID`.
    TAG_MUSIC_BRAINZ_CONDUCTOR_ID = "musicbrainz_conductorid",
    /// Go `TagMusicBrainzArrangerID`.
    TAG_MUSIC_BRAINZ_ARRANGER_ID = "musicbrainz_arrangerid",
    /// Go `TagMusicBrainzPerformerID`.
    TAG_MUSIC_BRAINZ_PERFORMER_ID = "musicbrainz_performerid",
}
