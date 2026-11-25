//! MusicBrainz disc lookups and tags for explicitly approved Browse exports.
use crate::{
    audio_disc::{AudioDisc, Toc},
    audio_export::Format,
    audio_services,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use lofty::{
    config::WriteOptions,
    picture::{Picture, PictureType},
    tag::{ItemKey, Tag, TagExt, TagType},
};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{io::Cursor, path::Path, time::Duration};
use tokio::{sync::Mutex, time::Instant};

const MAX_COVER: usize = 2 * 1024 * 1024;
static NEXT_REQUEST: Mutex<Option<Instant>> = Mutex::const_new(None);

fn disc_id(toc: &Toc) -> Result<String, String> {
    if toc.starts.is_empty()
        || toc.starts.len() > 99
        || toc.end > 450_000
        || toc.starts.last() >= Some(&toc.end)
        || toc.starts.windows(2).any(|a| a[0] >= a[1])
    {
        return Err("Invalid audio CD table of contents.".into());
    }
    use std::fmt::Write as _;
    let mut hex = format!("01{:02X}{:08X}", toc.starts.len(), toc.end + 150);
    for index in 0..99 {
        let sector = toc.starts.get(index).map_or(0, |n| n + 150);
        let _ = write!(hex, "{sector:08X}");
    }
    Ok(STANDARD
        .encode(Sha1::digest(hex))
        .replace('+', ".")
        .replace('/', "_")
        .replace('=', "-"))
}

#[derive(Serialize)]
pub(crate) struct Album {
    id: String,
    album: String,
    artist: String,
    date: String,
    country: String,
    label: String,
    catalog: String,
    disc: u32,
    disc_total: usize,
    tracks: Vec<AlbumTrack>,
}

#[derive(Serialize)]
struct AlbumTrack {
    number: usize,
    title: String,
    artist: String,
}

#[derive(Deserialize)]
struct DiscLookup {
    #[serde(default)]
    releases: Vec<Release>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Release {
    id: String,
    title: String,
    #[serde(default)]
    artist_credit: Vec<Credit>,
    #[serde(default)]
    date: String,
    #[serde(default)]
    country: String,
    #[serde(default)]
    label_info: Vec<LabelInfo>,
    #[serde(default)]
    media: Vec<Medium>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct LabelInfo {
    label: Option<Name>,
    catalog_number: Option<String>,
}

#[derive(Deserialize)]
struct Name {
    name: String,
}

#[derive(Deserialize)]
struct Credit {
    name: String,
    #[serde(default)]
    joinphrase: String,
}

#[derive(Deserialize)]
struct Medium {
    position: u32,
    #[serde(default)]
    discs: Vec<DiscId>,
    #[serde(default)]
    tracks: Vec<Track>,
}

#[derive(Deserialize)]
struct DiscId {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Track {
    position: usize,
    title: String,
    #[serde(default)]
    artist_credit: Vec<Credit>,
    recording: Option<Recording>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Recording {
    #[serde(default)]
    artist_credit: Vec<Credit>,
}

fn credits(items: &[Credit]) -> String {
    let mut name = String::new();
    for credit in items {
        name.push_str(&credit.name);
        name.push_str(&credit.joinphrase);
    }
    name
}

fn release_id(id: &str) -> bool {
    id.split('-').map(|s| s.len()).eq([8, 4, 4, 4, 12])
        && id.bytes().all(|ch| ch == b'-' || ch.is_ascii_hexdigit())
}

fn albums(lookup: DiscLookup, id: &str, count: usize) -> Vec<Album> {
    let mut albums = Vec::new();
    for release in lookup.releases {
        if !release_id(&release.id) {
            continue;
        }
        for medium in &release.media {
            if !medium.discs.iter().any(|disc| disc.id == id)
                || medium.tracks.len() != count
                || medium
                    .tracks
                    .iter()
                    .enumerate()
                    .any(|(index, t)| t.position != index + 1)
            {
                continue;
            }
            let tracks = medium
                .tracks
                .iter()
                .map(|track| {
                    let artist = if track.artist_credit.is_empty() {
                        track
                            .recording
                            .as_ref()
                            .map_or_else(String::new, |r| credits(&r.artist_credit))
                    } else {
                        credits(&track.artist_credit)
                    };
                    AlbumTrack {
                        number: track.position,
                        title: track.title.clone(),
                        artist,
                    }
                })
                .collect();
            albums.push(Album {
                id: release.id.clone(),
                album: release.title.clone(),
                artist: credits(&release.artist_credit),
                date: release.date.clone(),
                country: release.country.clone(),
                label: release
                    .label_info
                    .iter()
                    .find_map(|l| l.label.as_ref())
                    .map(|l| l.name.clone())
                    .unwrap_or_default(),
                catalog: release
                    .label_info
                    .iter()
                    .find_map(|l| l.catalog_number.clone())
                    .unwrap_or_default(),
                disc: medium.position,
                disc_total: release.media.len(),
                tracks,
            });
        }
    }
    albums
}

#[tauri::command]
pub(crate) async fn lookup_audio_releases(
    image_path: Option<String>,
    starts: Option<Vec<u64>>,
    lead_out: Option<u64>,
) -> Result<Vec<Album>, String> {
    let toc = match (image_path, starts, lead_out) {
        (Some(path), None, None) => AudioDisc::load(Path::new(&path))?.toc,
        (None, Some(starts), Some(end)) => Toc { starts, end },
        _ => return Err("A complete audio CD table of contents is required.".into()),
    };
    let id = disc_id(&toc)?;
    let response = request_release(&id).await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let bytes = audio_services::body(
        response.error_for_status().map_err(|e| e.to_string())?,
        2 * 1024 * 1024,
    )
    .await?;
    let lookup =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid MusicBrainz response: {e}"))?;
    Ok(albums(lookup, &id, toc.starts.len()))
}

async fn request_release(id: &str) -> Result<reqwest::Response, String> {
    let mut next = NEXT_REQUEST.lock().await;
    let mut retried = false;
    loop {
        if let Some(time) = *next {
            if time > Instant::now() + Duration::from_secs(3) {
                return Err("MusicBrainz is busy. Try again later.".into());
            }
            tokio::time::sleep_until(time).await;
        }
        *next = Some(Instant::now() + Duration::from_millis(1050));
        let response = audio_services::client(false)?
            .get(format!("https://musicbrainz.org/ws/2/discid/{id}"))
            .query(&[
                ("fmt", "json"),
                ("cdstubs", "no"),
                ("inc", "artist-credits+labels+recordings"),
            ])
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !matches!(response.status().as_u16(), 429 | 503) {
            return Ok(response);
        }
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5)
            .clamp(1, 86400);
        *next = Some(Instant::now() + Duration::from_secs(seconds) + Duration::from_millis(50));
        if retried {
            return Err("MusicBrainz is busy. Try again later.".into());
        }
        // A brief Retry-After can recover a transient overload without keeping
        // the album editor spinning for an open-ended server pause.
        retried = true;
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Cover {
    mime: String,
    data: String,
}

fn cover_mime(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Ok("image/jpeg");
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    Err("The front cover is not a JPEG or PNG image.".into())
}

#[tauri::command]
pub(crate) async fn fetch_album_cover(id: String) -> Result<Option<Cover>, String> {
    if !release_id(&id) {
        return Err("Invalid MusicBrainz release ID.".into());
    }
    let response = audio_services::client(true)?
        .get(format!(
            "https://coverartarchive.org/release/{id}/front-500"
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let bytes = audio_services::body(
        response.error_for_status().map_err(|e| e.to_string())?,
        MAX_COVER,
    )
    .await?;
    let mime = cover_mime(&bytes)?.into();
    Ok(Some(Cover {
        mime,
        data: STANDARD.encode(bytes),
    }))
}

#[derive(Clone, Deserialize)]
pub(crate) struct TrackTags {
    album: String,
    album_artist: String,
    title: String,
    artist: String,
    date: String,
    disc: Option<u32>,
    disc_total: Option<u32>,
    track: u32,
    track_total: u32,
    cover: Option<Cover>,
}

#[derive(Deserialize)]
pub(crate) struct AlbumTags {
    pub album: String,
    pub artist: String,
    pub date: String,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub tracks: Vec<CueTrackTags>,
    pub cover: Option<Cover>,
}

#[derive(Deserialize)]
pub(crate) struct CueTrackTags {
    pub number: u32,
    pub title: String,
    pub artist: String,
}

impl AlbumTags {
    pub(crate) fn track(&self, number: u32) -> Option<TrackTags> {
        let track = self.tracks.iter().find(|track| track.number == number)?;
        Some(TrackTags {
            album: self.album.clone(),
            album_artist: self.artist.clone(),
            title: track.title.clone(),
            artist: if track.artist.trim().is_empty() { self.artist.clone() } else { track.artist.clone() },
            date: self.date.clone(),
            disc: self.disc,
            disc_total: self.disc_total,
            track: number,
            track_total: self.tracks.len() as u32,
            cover: self.cover.clone(),
        })
    }
}

pub(crate) fn write_album_tags(path: &Path, values: &AlbumTags) -> Result<(), String> {
    write_tags(path, Format::Flac, &TrackTags {
        album: values.album.clone(),
        album_artist: values.artist.clone(),
        title: String::new(),
        artist: values.artist.clone(),
        date: values.date.clone(),
        disc: values.disc,
        disc_total: values.disc_total,
        track: 0,
        track_total: 0,
        cover: values.cover.clone(),
    })
}

pub(crate) fn write_tags(path: &Path, format: Format, values: &TrackTags) -> Result<(), String> {
    let mut tag = Tag::new(match format {
        Format::Flac => TagType::VorbisComments,
        Format::Mp3 | Format::Wav => TagType::Id3v2,
    });
    for (field, value) in [
        (ItemKey::AlbumTitle, &values.album),
        (ItemKey::AlbumArtist, &values.album_artist),
        (ItemKey::TrackTitle, &values.title),
        (ItemKey::TrackArtist, &values.artist),
        (ItemKey::RecordingDate, &values.date),
    ] {
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err("Invalid audio tag text.".into());
        }
        if !value.trim().is_empty() {
            tag.insert_text(field, value.trim().into());
        }
    }
    for (field, value) in [
        (ItemKey::TrackNumber, Some(values.track)),
        (ItemKey::TrackTotal, Some(values.track_total)),
        (ItemKey::DiscNumber, values.disc),
        (ItemKey::DiscTotal, values.disc_total),
    ] {
        if let Some(n) = value.filter(|n| *n > 0) {
            tag.insert_text(field, n.to_string());
        }
    }
    if let Some(cover) = &values.cover {
        if cover.data.len() > MAX_COVER.div_ceil(3) * 4 {
            return Err("The cover image is too large.".into());
        }
        let bytes = STANDARD.decode(&cover.data).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_COVER || cover_mime(&bytes)? != cover.mime {
            return Err("Invalid cover image.".into());
        }
        let mut pic = Picture::from_reader(&mut Cursor::new(bytes)).map_err(|e| e.to_string())?;
        pic.set_pic_type(PictureType::CoverFront);
        tag.push_picture(pic);
    }
    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "uses the public MusicBrainz example disc"]
    async fn public_musicbrainz_returns_release_candidates() {
        let release = lookup_audio_releases(
            None,
            Some(vec![0, 15213, 32164, 46442, 63264, 80339]),
            Some(95312),
        )
        .await
        .unwrap();
        assert!(release.iter().any(|r| r.tracks.len() == 6));
    }

    #[tokio::test]
    #[ignore = "downloads the public example Cover Art Archive thumbnail"]
    async fn public_cover_art_archive_returns_a_front_cover() {
        let cover = fetch_album_cover("76df3287-6cda-33eb-8e9a-044b5e15ffdd".into())
            .await
            .unwrap()
            .unwrap();
        assert!(cover.mime == "image/jpeg" || cover.mime == "image/png");
    }

    #[test]
    fn exact_disc_id_selects_the_matching_medium_and_preserves_artist_credits() {
        let first = "76df3287-6cda-33eb-8e9a-044b5e15ffdd";
        let second = "d3dc4be9-9749-4959-99e5-133d0cb467fe";
        let releases = [first, second].iter().map(|id| serde_json::json!({
                "id": id, "title": "Album", "country": "GB", "artist-credit": [
                    {"name": "A", "joinphrase": " & "}, {"name": "B"}
                ],
                "media": [
                    {"position": 1, "discs": [{"id": "Different"}], "tracks": [{"position": 1, "title": "Skip"}]},
                    {"position": 2, "discs": [{"id": "Input"}], "tracks": [{"position": 1, "title": "Keep",
                        "recording": {"artist-credit": [{"name": "Guest"}]}}]}
                ]
            })).collect::<Vec<_>>();
        let lookup = serde_json::from_value(serde_json::json!({ "releases": releases })).unwrap();
        let results = albums(lookup, "Input", 1);
        assert_eq!(results.len(), 2);
        for album in results {
            assert_eq!(album.disc, 2);
            assert_eq!(album.artist, "A & B");
            assert_eq!(album.tracks[0].title, "Keep");
            assert_eq!(album.tracks[0].artist, "Guest");
        }
        assert!(albums(
            DiscLookup {
                releases: Vec::new()
            },
            "Input",
            1
        )
        .is_empty());
    }

    #[test]
    fn musicbrainz_published_toc_example() {
        let toc = Toc {
            starts: vec![0, 15213, 32164, 46442, 63264, 80339],
            end: 95312,
        };
        assert_eq!(disc_id(&toc).unwrap(), "49HHV7Eb8UKF3aQiNmu1GR8vKTY-");
    }
}
