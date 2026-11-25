//! Encode CD audio using the same PCM path for image exports and physical reads.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

const RECORD: &str = ".disc-xplorer-audio.json";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Format {
    #[default]
    Wav,
    Flac,
    Mp3,
}

impl Format {
    pub(crate) fn from_extension(value: &str) -> Self {
        match value {
            "flac" => Self::Flac,
            "mp3" => Self::Mp3,
            _ => Self::Wav,
        }
    }

    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
    }

    pub(crate) fn encode(self, source: impl Read, bytes: u64, path: &Path) -> Result<(), String> {
        if bytes % 4 != 0 {
            return Err("Audio input does not contain whole stereo samples.".into());
        }
        match self {
            Self::Wav => super::encode_audio_wav(source, bytes, path),
            Self::Flac => super::encode_audio_flac(source, bytes, path),
            Self::Mp3 => super::encode_audio_mp3(source, bytes, path),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExportMode {
    #[default]
    Off,
    AudioOnly,
    AnyCd,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Retention {
    #[default]
    Keep,
    ReplaceAudioOnly,
}

pub(crate) struct TempDir(pub PathBuf);

impl TempDir {
    pub(crate) fn create(parent: &Path) -> Result<Self, String> {
        for _ in 0..32 {
            let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let time = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = parent.join(format!(
                ".disc-xplorer-audio-{}-{time}-{id}",
                std::process::id()
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("Could not create a temporary audio folder.".into())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) struct CheckedRead<R, F>(pub R, pub F);

impl<R: Read, F: FnMut() -> Result<(), String>> Read for CheckedRead<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (self.1)().map_err(io::Error::other)?;
        self.0.read(buf)
    }
}

#[cfg(windows)]
fn move_file(source: &Path, dest: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let wide_path = |path: &Path| {
        let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Path contains a null character.",
            ));
        }
        wide.push(0);
        Ok(wide)
    };
    let source = wide_path(source)?;
    let dest = wide_path(dest)?;
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // Both paths are null-terminated buffers that remain alive during the call.
    if unsafe { MoveFileExW(source.as_ptr(), dest.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Publish a finished file without replacing a file that appeared at the destination.
#[cfg(windows)]
pub(crate) fn publish_file(source: &Path, dest: &Path) -> io::Result<()> {
    move_file(source, dest, false)
}

#[cfg(not(windows))]
pub(crate) fn publish_file(source: &Path, dest: &Path) -> io::Result<()> {
    match fs::hard_link(source, dest) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
        Err(_) => {}
    }
    // Some removable or network drives do not support hard links.
    let mut input = File::open(source)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    let result = io::copy(&mut input, &mut output).and_then(|_| output.sync_all());
    drop(output);
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct Stamp {
    name: String,
    length: u64,
    modified: SystemTime,
}

fn leaf(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.contains(['/', '\\', ':'])
}

impl Stamp {
    fn read(folder: &Path, name: &str) -> Option<Self> {
        if !leaf(name) {
            return None;
        }
        let meta = fs::symlink_metadata(folder.join(name)).ok()?;
        (meta.is_file() && meta.len() > 0).then_some(Self {
            name: name.into(),
            length: meta.len(),
            modified: meta.modified().ok()?,
        })
    }

    fn matches(&self, folder: &Path) -> bool {
        Self::read(folder, &self.name).as_ref() == Some(self)
    }
}

pub(crate) fn sync_encoded_file(path: &Path) -> io::Result<()> {
    // FlushFileBuffers requires a writable handle on Windows.
    fs::OpenOptions::new().write(true).open(path)?.sync_all()
}

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|folder| folder.sync_all())
        .map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Album {
    version: u8,
    dump_name: String,
    directory: String,
    tracks: Vec<Stamp>,
    replaced_image: bool,
}

impl Album {
    pub(crate) fn path(&self, output: &Path) -> PathBuf {
        output.join(&self.directory)
    }

    pub(crate) fn replaced_image(&self) -> bool {
        self.replaced_image
    }

    pub(crate) fn load(output: &Path, name: &str) -> Option<Self> {
        let record = output.join(RECORD);
        let meta = fs::symlink_metadata(&record).ok()?;
        if !meta.is_file() || meta.len() > 128 * 1024 {
            return None;
        }
        let album: Self = serde_json::from_slice(&fs::read(record).ok()?).ok()?;
        if album.version != 1
            || album.dump_name != name
            || !leaf(&album.directory)
            || album.tracks.is_empty()
        {
            return None;
        }
        album.check_files(output, false).then_some(album)
    }

    fn check_files(&self, output: &Path, require_matching_stamps: bool) -> bool {
        let folder = self.path(output);
        fs::symlink_metadata(&folder).is_ok_and(|meta| meta.is_dir())
            && self.tracks.iter().all(|track| {
                Stamp::read(&folder, &track.name)
                    .is_some_and(|current| !require_matching_stamps || current == *track)
            })
    }

    pub(crate) fn save(&self, output: &Path) -> Result<(), String> {
        let dest = output.join(RECORD);
        if fs::symlink_metadata(&dest).is_ok() {
            let existing = Self::load(output, &self.dump_name);
            // An older album may have been moved or modified, but the record
            // still belongs to us if its version and dump name can be read.
            if existing.is_none() {
                let recognized = fs::symlink_metadata(&dest)
                    .ok()
                    .filter(|m| m.is_file() && m.len() <= 128 * 1024)
                    .and_then(|_| fs::read(&dest).ok())
                    .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
                    .is_some_and(|record| {
                        record.version == 1 && record.dump_name == self.dump_name
                    });
                if !recognized {
                    return Err("Existing audio export record is unrecognized.".into());
                }
            }
        }
        let temp = TempDir::create(output)?;
        let path = temp.0.join("album.json");
        let mut file = File::create(&path).map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut file, self).map_err(|e| e.to_string())?;
        file.flush()
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        #[cfg(windows)]
        move_file(&path, &dest, true).map_err(|error| error.to_string())?;
        #[cfg(not(windows))]
        fs::rename(path, dest).map_err(|error| error.to_string())?;
        sync_directory(output)
    }

    pub(crate) fn mark_replacement(&mut self, output: &Path) -> Result<(), String> {
        if !self.check_files(output, true) {
            return Err("Exported audio changed before image cleanup.".into());
        }
        sync_directory(&self.path(output))?;
        self.replaced_image = true;
        self.save(output)
    }
}

pub(crate) struct OriginalImage(Vec<Stamp>);

impl OriginalImage {
    /// Pin only the CUE and the exact BIN files it addresses inside the dump.
    fn capture(output: &Path, cue: &Path, name: &str, tracks: &[super::TrackInfo]) -> Option<Self> {
        let cue_leaf = cue.file_name()?.to_str()?;
        if cue_leaf != format!("{name}.cue") {
            return None;
        }
        let mut files = BTreeSet::new();
        for track in tracks {
            let path = Path::new(&track.bin_path);
            let file = path.file_name()?.to_str()?;
            let owned = file == format!("{name}.bin")
                || file
                    .strip_prefix(&format!("{name} (Track "))
                    .and_then(|s| s.strip_suffix(").bin"))
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
            if path.parent()? != output || !owned {
                return None;
            }
            files.insert(file);
        }
        let mut stamps: Vec<_> = files
            .into_iter()
            .map(|file| Stamp::read(output, file))
            .collect::<Option<_>>()?;
        // Remove the descriptor only after the final BIN has been removed.
        stamps.push(Stamp::read(output, cue_leaf)?);
        Some(Self(stamps))
    }

    fn check_coverage(
        &self,
        output: &Path,
        mut ranges: BTreeMap<String, Vec<(u64, u64)>>,
    ) -> Result<(), String> {
        // The descriptor is the final stamp. Every BIN byte must occur once in
        // the selected ranges, including any pregap stored in the image.
        for file in &self.0[..self.0.len() - 1] {
            let path = output.join(&file.name);
            let Some(mut spans) = path.to_str().and_then(|path| ranges.remove(path)) else {
                return Err("The audio export omits sectors from the original image.".into());
            };
            spans.sort_unstable();
            let mut end = 0;
            for (start, length) in spans {
                if start != end {
                    return Err(
                        "The audio export omits or repeats sectors from the original image.".into(),
                    );
                }
                end = start + length;
            }
            if end != file.length {
                return Err("The audio export omits sectors from the original image.".into());
            }
        }
        Ok(())
    }

    pub(crate) fn check_companion_cue(&self, output: &Path, name: &str) -> Result<(), String> {
        let prefix = format!("{name}_");
        for entry in fs::read_dir(output).map_err(|error| error.to_string())? {
            let file = entry.map_err(|error| error.to_string())?.file_name();
            // Redumper writes another CUE for each additional CD-TEXT
            // language. Keep its image so the companion CUE still works.
            if file
                .to_str()
                .and_then(|file| file.strip_prefix(&prefix))
                .and_then(|tail| tail.strip_suffix(".cue"))
                .is_some_and(|language| {
                    language.len() == 2 && language.bytes().all(|b| b.is_ascii_hexdigit())
                })
            {
                return Err("The image has an additional CD-TEXT cue sheet.".into());
            }
        }
        Ok(())
    }

    pub(crate) fn check(&self, output: &Path) -> Result<(), String> {
        if fs::symlink_metadata(output).is_ok_and(|meta| meta.is_dir())
            && self.0.iter().all(|file| file.matches(output))
        {
            Ok(())
        } else {
            Err("The BIN/CUE image changed before cleanup.".into())
        }
    }

    pub(crate) fn remove(&self, output: &Path) -> Result<(), String> {
        self.check(output)?;
        for file in &self.0 {
            if !file.matches(output) {
                return Err("The BIN/CUE image changed during cleanup.".into());
            }
            fs::remove_file(output.join(&file.name)).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

pub(crate) struct Exported {
    pub album: Album,
    pub original: Result<Option<OriginalImage>, String>,
}

pub(crate) struct TrackLayout<'a> {
    cue: &'a Path,
    tracks: Vec<super::TrackInfo>,
    audio: Vec<usize>,
}

impl<'a> TrackLayout<'a> {
    pub(crate) fn load(cue: &'a Path, mode: ExportMode) -> Result<Option<Self>, String> {
        if mode == ExportMode::Off {
            return Ok(None);
        }
        let cue_path = cue.to_str().ok_or("Invalid CUE path.")?;
        let tracks = super::get_cue_tracks(cue_path.into())?;
        let audio: Vec<_> = tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| (!track.is_data && track.mode == "AUDIO").then_some(index))
            .collect();
        let layout = Self { cue, tracks, audio };
        if layout.audio.is_empty() || (mode == ExportMode::AudioOnly && !layout.pure_audio()) {
            return Ok(None);
        }
        Ok(Some(layout))
    }

    pub(crate) fn pure_audio(&self) -> bool {
        self.audio.len() == self.tracks.len()
    }

    pub(crate) fn export(
        self,
        name: &str,
        format: Format,
        gaps: Option<&str>,
        allow_replacement: bool,
        mut check: impl FnMut() -> Result<(), String>,
        mut progress: impl FnMut(u32, usize, usize),
    ) -> Result<Exported, String> {
        check()?;
        let pure_audio = self.pure_audio();
        let Self { cue, tracks, audio } = self;
        let output = cue.parent().ok_or("Missing dump folder.")?;
        let mut original = if allow_replacement && pure_audio {
            OriginalImage::capture(output, cue, name, &tracks)
                .map(Some)
                .ok_or_else(|| "Cannot verify ownership of the BIN/CUE files.".to_owned())
        } else {
            Ok(None)
        };
        let mut coverage = BTreeMap::new();
        let stage = TempDir::create(output)?;
        let mut filenames = BTreeSet::new();
        for (position, &idx) in audio.iter().enumerate() {
            check()?;
            let track = &tracks[idx];
            progress(track.number, position + 1, audio.len());
            let leaf = format!("Track {:02}.{}", track.number, format.extension());
            if !filenames.insert(leaf.clone()) || track.number == 0 {
                return Err("The CUE has invalid or duplicate audio track numbers.".into());
            }
            let segments = super::audio_segments(&tracks, idx, super::GapMode::parse(gaps));
            if matches!(original, Ok(Some(_))) {
                for (bin, start, length) in &segments {
                    coverage
                        .entry(bin.clone())
                        .or_insert_with(Vec::new)
                        .push((*start, *length));
                }
            }
            let (source, bytes) = super::open_audio_segments(segments)?;
            if bytes == 0 {
                return Err(format!("Track {} has no audio samples.", track.number));
            }
            let file_size = fs::metadata(&track.bin_path)
                .map_err(|error| error.to_string())?
                .len();
            if file_size % super::RAW_SECTOR_SIZE != 0 {
                return Err(format!("Track {} has a partial sector.", track.number));
            }
            let path = stage.0.join(&leaf);
            format.encode(CheckedRead(source, &mut check), bytes, &path)?;
            sync_encoded_file(&path).map_err(|e| e.to_string())?;
        }
        if let Ok(Some(image)) = &original {
            image.check(output)?;
            if let Err(reason) = image
                .check_coverage(output, coverage)
                .and_then(|()| image.check_companion_cue(output, name))
            {
                original = Err(reason);
            }
        }
        check()?;
        let (directory, dest) = (1..=1000)
            .find_map(|n| {
                let name = if n == 1 {
                    "Audio Tracks".into()
                } else {
                    format!("Audio Tracks ({n})")
                };
                let path = output.join(&name);
                match fs::create_dir(&path) {
                    Ok(()) => Some(Ok((name, path))),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(e) => Some(Err(e.to_string())),
                }
            })
            .ok_or("Could not reserve an audio output folder.")??;
        let mut published = Vec::new();
        let result = (|| {
            for leaf in filenames {
                check()?;
                publish_file(&stage.0.join(&leaf), &dest.join(&leaf)).map_err(|e| e.to_string())?;
                published.push(leaf);
            }
            let stamps = published
                .iter()
                .map(|leaf| Stamp::read(&dest, leaf))
                .collect::<Option<Vec<_>>>()
                .ok_or("Could not verify saved audio files.")?;
            let album = Album {
                version: 1,
                dump_name: name.into(),
                directory,
                tracks: stamps,
                replaced_image: false,
            };
            check()?;
            album.save(output)?;
            Ok(Exported { album, original })
        })();
        if result.is_err() {
            for leaf in published {
                let _ = fs::remove_file(dest.join(leaf));
            }
            let _ = fs::remove_dir(dest);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECTOR: usize = 2352;

    fn fixture(mode: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::create(&std::env::temp_dir()).unwrap();
        let cue = dir.0.join("Album.cue");
        let sheet = format!("FILE \"Album.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 {mode}\n    INDEX 00 00:00:03\n    INDEX 01 00:00:04\n");
        fs::write(&cue, sheet).unwrap();
        let pcm: Vec<_> = (0..10).flat_map(|sector| vec![sector; SECTOR]).collect();
        fs::write(dir.0.join("Album.bin"), pcm).unwrap();
        (dir, cue)
    }

    fn export(
        cue: &Path,
        mode: ExportMode,
        format: Format,
        replace: bool,
    ) -> Result<Option<Exported>, String> {
        TrackLayout::load(cue, mode)?
            .map(|layout| {
                layout.export(
                    "Album",
                    format,
                    Some("next"),
                    replace,
                    || Ok(()),
                    |_, _, _| {},
                )
            })
            .transpose()
    }

    #[test]
    fn album_policy_skips_off_and_never_replaces_mixed_mode() {
        let (dir, cue) = fixture("MODE1/2352");
        assert!(export(&cue, ExportMode::Off, Format::Wav, true)
            .unwrap()
            .is_none());
        assert!(export(&cue, ExportMode::AudioOnly, Format::Wav, true)
            .unwrap()
            .is_none());
        assert!(Album::load(&dir.0, "Album").is_none());

        let audio = export(&cue, ExportMode::AnyCd, Format::Wav, true)
            .unwrap()
            .unwrap();
        assert!(audio.original.unwrap().is_none());
        let folder = audio.album.path(&dir.0);
        assert!(folder.join("Track 01.wav").is_file());
        assert!(!folder.join("Track 02.wav").exists());
        assert!(cue.is_file() && cue.with_extension("bin").is_file());
    }

    #[test]
    fn lossless_export_uses_the_selected_gap_layout_without_repeating_the_shared_bin() {
        let (dir, cue) = fixture("AUDIO");
        fs::create_dir(dir.0.join("Audio Tracks")).unwrap();
        fs::write(dir.0.join("Audio Tracks/keep.txt"), "user file").unwrap();

        let audio = export(&cue, ExportMode::AudioOnly, Format::Wav, false)
            .unwrap()
            .unwrap();
        assert!(audio.original.unwrap().is_none());
        let folder = audio.album.path(&dir.0);
        assert_eq!(folder.file_name().unwrap(), "Audio Tracks (2)");
        let first = fs::read(folder.join("Track 01.wav")).unwrap();
        let second = fs::read(folder.join("Track 02.wav")).unwrap();
        assert_eq!(first.len(), 44 + 3 * SECTOR);
        assert_eq!(second.len(), 44 + 7 * SECTOR);
        let full = fs::read(cue.with_extension("bin")).unwrap();
        assert_eq!([&first[44..], &second[44..]].concat(), full);
        assert!(dir.0.join("Audio Tracks/keep.txt").is_file());
    }

    #[test]
    fn replacement_can_be_reopened_after_bins_are_removed() {
        let (dir, cue) = fixture("AUDIO");
        let mut audio = export(&cue, ExportMode::AudioOnly, Format::Flac, true)
            .unwrap()
            .unwrap();
        let original = audio.original.as_ref().unwrap().as_ref().unwrap();
        assert!(!Album::load(&dir.0, "Album").unwrap().replaced_image());

        let flac = fs::read(audio.album.path(&dir.0).join("Track 01.flac")).unwrap();
        assert_eq!(&flac[..4], b"fLaC");
        // STREAMINFO stores the expected PCM sample count in the low 36 bits.
        let sample_total = u64::from_be_bytes(flac[18..26].try_into().unwrap()) & ((1 << 36) - 1);
        assert_eq!(sample_total, 3 * 588);
        use md5::{Digest, Md5};
        let bin = fs::read(cue.with_extension("bin")).unwrap();
        assert_eq!(&flac[26..42], Md5::digest(&bin[..3 * SECTOR]).as_slice());

        fs::write(dir.0.join("Album.log"), "kept").unwrap();
        fs::write(dir.0.join("Album (Track A).bin"), "kept").unwrap();
        original.check(&dir.0).unwrap();
        audio.album.mark_replacement(&dir.0).unwrap();
        original.remove(&dir.0).unwrap();
        assert!(!cue.exists() && !cue.with_extension("bin").exists());
        assert!(dir.0.join("Album.log").is_file() && dir.0.join("Album (Track A).bin").is_file());
        let album = Album::load(&dir.0, "Album").unwrap();
        assert!(album.replaced_image());
        let track = album.path(&dir.0).join("Track 02.flac");
        assert!(track.is_file());
        // Editing audio metadata after the dump should leave the album accessible.
        fs::OpenOptions::new()
            .append(true)
            .open(&track)
            .unwrap()
            .write_all(b"tag")
            .unwrap();
        assert!(Album::load(&dir.0, "Album").is_some());
        fs::remove_file(track).unwrap();
        assert!(Album::load(&dir.0, "Album").is_none());
    }

    #[test]
    fn cancellation_or_record_failure_leaves_the_original_data() {
        let (dir, cue) = fixture("AUDIO");
        let cancelled = std::cell::Cell::new(false);
        let layout = TrackLayout::load(&cue, ExportMode::AudioOnly)
            .unwrap()
            .unwrap();
        let result = layout.export(
            "Album",
            Format::Mp3,
            None,
            true,
            || {
                if cancelled.get() {
                    Err("cancelled".into())
                } else {
                    Ok(())
                }
            },
            |_, position, _| {
                if position == 2 {
                    cancelled.set(true);
                }
            },
        );
        assert!(
            result.is_err(),
            "cancellation during the second encode must stop the album"
        );
        assert!(!dir.0.join("Audio Tracks").exists());
        assert!(Album::load(&dir.0, "Album").is_none());

        fs::write(dir.0.join(RECORD), "unrelated file").unwrap();
        assert!(export(&cue, ExportMode::AudioOnly, Format::Mp3, true).is_err());
        assert!(!dir.0.join("Audio Tracks").exists());
        assert_eq!(
            fs::read_to_string(dir.0.join(RECORD)).unwrap(),
            "unrelated file"
        );
        assert!(cue.is_file() && cue.with_extension("bin").is_file());
    }

    #[test]
    fn an_export_without_pregaps_keeps_the_original_image() {
        let (dir, cue) = fixture("AUDIO");
        let layout = TrackLayout::load(&cue, ExportMode::AudioOnly)
            .unwrap()
            .unwrap();
        let audio = layout
            .export(
                "Album",
                Format::Wav,
                Some("leave-out"),
                true,
                || Ok(()),
                |_, _, _| {},
            )
            .unwrap();
        assert!(matches!(audio.original, Err(reason) if reason.contains("omits")));
        assert!(audio.album.path(&dir.0).join("Track 02.wav").is_file());
        assert!(cue.is_file() && cue.with_extension("bin").is_file());
    }

    #[test]
    fn a_second_cd_text_cue_keeps_its_shared_bin() {
        let (dir, cue) = fixture("AUDIO");
        let other_language = dir.0.join("Album_69.cue");
        fs::copy(&cue, &other_language).unwrap();
        let audio = export(&cue, ExportMode::AudioOnly, Format::Wav, true)
            .unwrap()
            .unwrap();
        assert!(matches!(audio.original, Err(reason) if reason.contains("additional CD-TEXT")));
        assert!(audio.album.path(&dir.0).join("Track 01.wav").is_file());
        assert!(cue.is_file() && other_language.is_file() && cue.with_extension("bin").is_file());
    }

    #[test]
    fn changed_bin_blocks_cleanup_and_mp3_is_finalized() {
        let (dir, cue) = fixture("AUDIO");
        let audio = export(&cue, ExportMode::AudioOnly, Format::Mp3, true)
            .unwrap()
            .unwrap();
        let mp3 = fs::read(audio.album.path(&dir.0).join("Track 01.mp3")).unwrap();
        assert!(
            mp3.len() > 1044,
            "the stream should include the final MPEG audio frames"
        );
        assert_eq!(mp3[0], 0xff);
        assert_eq!(mp3[1] & 0xe0, 0xe0);
        assert!(mp3.windows(4).any(|window| window == b"Info"));
        fs::write(cue.with_extension("bin"), "another image").unwrap();
        assert!(audio.original.unwrap().unwrap().remove(&dir.0).is_err());
        assert!(cue.is_file());
        assert_eq!(
            fs::read(cue.with_extension("bin")).unwrap(),
            b"another image"
        );
    }
}
