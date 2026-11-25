//! Decode the real export output with an independent decoder before packaging.
use super::save_audio_track;
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
