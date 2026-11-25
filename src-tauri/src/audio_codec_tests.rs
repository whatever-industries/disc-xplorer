//! Decode the real export output with an independent decoder before packaging.
use super::save_audio_track;
use crate::audio_export::{self, TempDir, TrackOutput};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, errors::Error, formats::FormatOptions,
    io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
};

const SECTORS: usize = 467;
const FRAMES: usize = SECTORS * 588;

struct AudioFixture {
    dir: PathBuf,
    pcm: Vec<i16>,
}

impl AudioFixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("dx-codec-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let pcm: Vec<_> = (0..FRAMES)
            .flat_map(|sample| {
                // Distinct signed waveforms in each channel. The fixture spans
                // more than one of the encoder's 1 MiB reads.
                let first = ((sample % 101) as i32 * 500 - 25_000) as i16;
                let second = ((sample % 163) as i32 * 280 - 22_000) as i16;
                [first, second]
            })
            .collect();
        let raw: Vec<_> = pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        fs::write(dir.join("Track.bin"), raw).unwrap();
        fs::write(
            dir.join("Disc.cue"),
            "FILE \"Track.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        Self { dir, pcm }
    }

    fn export(&self, format: &str) -> PathBuf {
        let dest = self.dir.join(format!("track.{format}"));
        save_audio_track(
            self.dir.join("Disc.cue").to_str().unwrap().into(),
            1,
            dest.to_str().unwrap().into(),
            format.into(),
            None,
        )
        .unwrap();
        dest
    }
}

impl Drop for AudioFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn decode(path: &Path) -> Vec<i16> {
    let extension = path.extension().unwrap().to_str().unwrap();
    let input = MediaSourceStream::new(Box::new(File::open(path).unwrap()), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let opts = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let mut format = symphonia::default::get_probe()
        .format(&hint, input, &opts, &MetadataOptions::default())
        .unwrap()
        .format;
    let track = format.default_track().unwrap();
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .unwrap();
    let mut samples = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => panic!("Could not read {extension} packet: {error}"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let buffer = decoder.decode(&packet).unwrap();
        assert_eq!(buffer.spec().rate, 44_100);
        assert_eq!(buffer.spec().channels.count(), 2);
        let mut interleaved = SampleBuffer::<i16>::new(buffer.capacity() as u64, *buffer.spec());
        interleaved.copy_interleaved_ref(buffer);
        samples.extend_from_slice(interleaved.samples());
    }
    samples
}

#[test]
fn flac_export_decodes_byte_for_byte() {
    let fixture = AudioFixture::new();
    assert!(decode(&fixture.export("flac")) == fixture.pcm);
}

#[test]
fn mp3_export_decodes_to_exact_track_length() {
    let fixture = AudioFixture::new();
    let decoded = decode(&fixture.export("mp3"));
    assert_eq!(decoded.len(), fixture.pcm.len());
    assert!(decoded.iter().any(|&sample| sample != 0));
}

#[test]
fn tags_and_front_cover_preserve_the_audio_for_all_three_formats() {
    use lofty::{
        file::TaggedFileExt,
        tag::{Accessor, TagType},
    };
    let fixture = AudioFixture::new();
    let tags = serde_json::from_value(serde_json::json!({
        "album": "Disc Xplorer test album", "album_artist": "Album artist",
        "title": "First track", "artist": "Track artist", "date": "1998-01-01",
        "disc": 1, "disc_total": 2, "track": 1, "track_total": 3,
        "cover": {
            "mime": "image/png",
            "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+cNf0AAAAASUVORK5CYII="
        }
    })).unwrap();
    fn wav_data(path: &Path) -> Vec<u8> {
        let wav = fs::read(path).unwrap();
        let mut position = 12;
        while position + 8 <= wav.len() {
            let len =
                u32::from_le_bytes(wav[position + 4..position + 8].try_into().unwrap()) as usize;
            if &wav[position..position + 4] == b"data" {
                return wav[position + 8..position + 8 + len].to_vec();
            }
            position += 8 + len + len % 2;
        }
        panic!("No WAV audio data");
    }

    for format in ["wav", "flac", "mp3"] {
        let reference = fixture.export(format);
        let dest = fixture.dir.join(format!("tagged.{format}"));
        crate::save_audio_track_inner(
            fixture.dir.join("Disc.cue").to_str().unwrap().into(),
            1,
            dest.to_str().unwrap().into(),
            format.into(),
            None,
            Some(&tags),
        )
        .unwrap();
        let tagged = lofty::read_from_path(&dest).unwrap();
        let kind = if format == "flac" {
            TagType::VorbisComments
        } else {
            TagType::Id3v2
        };
        let tag = tagged.tag(kind).unwrap();
        assert_eq!(tag.album().as_deref(), Some("Disc Xplorer test album"));
        assert_eq!(tag.title().as_deref(), Some("First track"));
        assert_eq!(tag.track(), Some(1));
        assert_eq!(tag.pictures().len(), 1);
        if format == "wav" {
            assert!(wav_data(&dest) == wav_data(&reference));
        } else {
            assert!(decode(&dest) == decode(&reference));
        }
    }
}

#[test]
fn paired_encoders_read_the_source_once_and_preserve_the_same_tags() {
    use lofty::{file::TaggedFileExt, tag::Accessor};
    use std::{cell::Cell, io::Read};

    let fixture = AudioFixture::new();
    let stage = TempDir::create(&fixture.dir).unwrap();
    let count = Cell::new(0);
    let mut source = File::open(fixture.dir.join("Track.bin")).unwrap();
    let measured = io::Read::by_ref(&mut source);
    struct Measured<'a, R> {
        count: &'a Cell<usize>,
        source: R,
    }
    impl<R: Read> Read for Measured<'_, R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let n = self.source.read(buffer)?;
            self.count.set(self.count.get() + n);
            Ok(n)
        }
    }
    let tags = serde_json::from_value(serde_json::json!({
        "album": "L’Été", "album_artist": "Test artist", "title": "Étoile",
        "artist": "Test artist", "date": "1998", "track": 1, "track_total": 3,
        "cover": { "mime": "image/png", "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+cNf0AAAAASUVORK5CYII=" },
    })).unwrap();
    let stem = audio_export::track_stem(1, Some("Étoile"));
    let bytes = fixture.pcm.len() * 2;
    let files = audio_export::stage_track(
        Measured {
            count: &count,
            source: measured,
        },
        bytes as u64,
        TrackOutput::FlacMp3,
        &stage.0,
        &stem,
        Some(&tags),
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(count.get(), bytes);
    assert_eq!(files, ["01 - Étoile.flac", "01 - Étoile.mp3"]);
    assert!(!stage.0.join("pcm.raw").exists());
    assert_eq!(decode(&stage.0.join(&files[0])), fixture.pcm);
    assert_eq!(
        decode(&stage.0.join(&files[1])),
        decode(&fixture.export("mp3"))
    );
    for file in files {
        let tagged = lofty::read_from_path(stage.0.join(file)).unwrap();
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.album().as_deref(), Some("L’Été"));
        assert_eq!(tag.title().as_deref(), Some("Étoile"));
        assert_eq!(tag.pictures().len(), 1);
    }
}

#[test]
fn flac_album_preserves_stored_audio_and_cue_indexes_in_shared_and_split_images() {
    let fixture = AudioFixture::new();
    let raw = fs::read(fixture.dir.join("Track.bin")).unwrap();
    let cue = fixture.dir.join("Disc.cue");
    let stage = TempDir::create(&fixture.dir).unwrap();
    let tags = serde_json::from_value(serde_json::json!({
        "album": "L’Été", "artist": "Test artist", "date": "1998", "tracks": [
            { "number": 1, "title": "Intro", "artist": "Test artist" },
            { "number": 2, "title": "Étoile", "artist": "Test artist" },
        ],
    }))
    .unwrap();
    fs::write(fixture.dir.join("First.bin"), &raw[..130 * 2352]).unwrap();
    fs::write(fixture.dir.join("Second.bin"), &raw[130 * 2352..]).unwrap();

    let first = "  TRACK 01 AUDIO\n    FLAGS PRE\n    ISRC USAAA9800001\n    INDEX 00 00:00:00\n    INDEX 01 00:00:05\n    INDEX 02 00:00:51\n";
    let second = "  TRACK 02 AUDIO\n    INDEX 00 00:01:55\n    INDEX 01 00:01:58\n";
    let layouts = [
        format!("FILE \"Track.bin\" BINARY\n{first}{second}"),
        format!(
            "FILE \"First.bin\" BINARY\n{first}FILE \"Second.bin\" BINARY\n{}",
            second
                .replace("00:01:55", "00:00:00")
                .replace("00:01:58", "00:00:03")
        ),
    ];
    for layout in layouts {
        fs::write(&cue, layout).unwrap();
        let files =
            audio_export::stage_album(&cue, &stage.0, "L’Été", Some(&tags), || Ok(()), || {})
                .unwrap();
        assert!(decode(&stage.0.join(&files[0])) == fixture.pcm);
        let companion = stage.0.join(&files[1]);
        let text = fs::read_to_string(&companion).unwrap();
        assert!(text.contains("FILE \"L’Été.flac\" WAVE\r\n"));
        assert!(text.contains(
            &first
                .replace('\n', "\r\n")
                .replace("  TRACK 01 AUDIO\r\n", "")
        ));
        assert!(text.contains("TITLE \"Étoile\""));
        assert!(text.contains("INDEX 00 00:01:55\r\n    INDEX 01 00:01:58"));
        if let Some(probe) = std::env::var_os("DX_TEST_FLAC_CUE_READER") {
            assert!(std::process::Command::new(probe)
                .arg(companion)
                .status()
                .unwrap()
                .success());
        }
    }
}
