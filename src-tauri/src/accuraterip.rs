//! Offline AccurateRip checksum core. The application has no database client
//! until Illustrate approves its access and confirms the connection details.
use crate::audio_disc::{AudioDisc, Toc, FRAMES_PER_SECTOR};
use std::{
    io::{self, Read},
    ops::Range,
};

const EDGE: i64 = 5 * FRAMES_PER_SECTOR as i64;
const MAX_SHIFT: i64 = EDGE - 1;

#[derive(Debug, PartialEq, Eq)]
struct DiscId {
    tracks: u32,
    offsets_sum: u32,
    weighted_offsets: u32,
    cddb: u32,
}

fn disc_id(toc: &Toc) -> DiscId {
    let tracks = toc.starts.len() as u32;
    let mut digit_sum = 0u32;
    let mut offsets_sum = toc.end as u32;
    let mut weighted_offsets = offsets_sum * (tracks + 1);
    for (index, &offset) in toc.starts.iter().enumerate() {
        let offset = offset as u32;
        offsets_sum += offset;
        weighted_offsets += offset.max(1) * (index as u32 + 1);
        let mut seconds = offset / 75 + 2;
        while seconds != 0 {
            digit_sum += seconds % 10;
            seconds /= 10;
        }
    }
    let seconds = (toc.end / 75 - toc.starts[0] / 75) as u32;
    let cddb = (digit_sum % 255) << 24 | seconds << 8 | tracks;
    DiscId {
        tracks,
        offsets_sum,
        weighted_offsets,
        cddb,
    }
}

#[derive(Debug)]
struct Checksums {
    v2: u32,
    /// V1 at every supported shift, including zero. Positive shifts read later samples.
    v1: Vec<(i32, u32)>,
}

fn verify_image_track(
    disc: &AudioDisc,
    number: u32,
    check: impl FnMut() -> io::Result<()>,
) -> io::Result<(DiscId, Checksums)> {
    let index = number
        .checked_sub(1)
        .map(|n| n as usize)
        .filter(|&i| i < disc.toc.starts.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid audio track number"))?;
    let first = disc.toc.starts[0];
    let start = (disc.toc.starts[index] - first) * FRAMES_PER_SECTOR;
    let end = (disc
        .toc
        .starts
        .get(index + 1)
        .copied()
        .unwrap_or(disc.toc.end)
        - first)
        * FRAMES_PER_SECTOR;
    let scoring = TrackScoring::new(
        start..end,
        0..disc.frames(),
        index == 0,
        index + 1 == disc.toc.starts.len(),
        MAX_SHIFT,
    )?;
    let checksums = scoring.checksums(disc.pcm(scoring.read_range())?, check)?;
    disc.check_files()?;
    Ok((disc_id(&disc.toc), checksums))
}

struct TrackScoring {
    track_start: i64,
    start: i64,
    end: i64,
    min_shift: i64,
    max_shift: i64,
}

impl TrackScoring {
    fn new(
        track: Range<u64>,
        available: Range<u64>,
        first: bool,
        last: bool,
        radius: i64,
    ) -> io::Result<Self> {
        let track_start = track.start as i64;
        let start = track_start + if first { EDGE - 1 } else { 0 };
        let end = track.end as i64 - if last { EDGE } else { 0 };
        if end <= start || start < available.start as i64 || end > available.end as i64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Incomplete AccurateRip sample range",
            ));
        }
        Ok(Self {
            track_start,
            start,
            end,
            min_shift: (-radius).max(available.start as i64 - start),
            max_shift: radius.min(available.end as i64 - end),
        })
    }

    fn read_range(&self) -> Range<u64> {
        (self.start + self.min_shift) as u64..(self.end + self.max_shift) as u64
    }

    fn checksums(
        &self,
        mut pcm: impl Read,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Checksums> {
        let count = (self.max_shift - self.min_shift + 1) as usize;
        let mut starts = Vec::with_capacity(count);
        let mut ends = Vec::with_capacity(count);
        let mut prefix = (0u32, 0u32);
        let mut v2 = 0u32;
        let mut buf = [0; 65536];
        let range = self.read_range();
        let mut position = range.start as i64;
        let last = range.end as i64;
        while position < last {
            check()?;
            let samples = ((last - position) as usize).min(buf.len() / 4);
            pcm.read_exact(&mut buf[..samples * 4])?;
            for word in buf[..samples * 4].chunks_exact(4) {
                if (self.min_shift..=self.max_shift).contains(&(position - self.start)) {
                    starts.push(prefix);
                }
                if (self.min_shift..=self.max_shift).contains(&(position - self.end)) {
                    ends.push(prefix);
                }
                let sample = u32::from_le_bytes(word.try_into().unwrap());
                prefix.0 = prefix.0.wrapping_add(sample);
                prefix.1 = prefix
                    .1
                    .wrapping_add(sample.wrapping_mul(position as u32 + 1));
                if (self.start..self.end).contains(&position) {
                    let product = u64::from(sample) * (position - self.track_start + 1) as u64;
                    v2 = v2
                        .wrapping_add(product as u32)
                        .wrapping_add((product >> 32) as u32);
                }
                position += 1;
            }
        }
        ends.push(prefix);
        // A prefix at the last byte can only terminate a scored range. To move
        // its multiplier back to track-relative positions, subtract the new
        // track origin times the unweighted sample sum.
        let v1 = (self.min_shift..=self.max_shift)
            .zip(starts.into_iter().zip(ends))
            .map(|(shift, (start, end))| {
                let sum = end.0.wrapping_sub(start.0);
                let weighted = end.1.wrapping_sub(start.1);
                let checksum =
                    weighted.wrapping_sub(sum.wrapping_mul((self.track_start + shift) as u32));
                (shift as i32, checksum)
            })
            .collect();
        Ok(Checksums { v1, v2 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_export::TempDir;
    use std::{fs, path::PathBuf};

    fn fixture(shift: i32, damaged: bool) -> (TempDir, PathBuf) {
        let dir = TempDir::create(&std::env::temp_dir()).unwrap();
        let cue = dir.0.join("Disc.cue");
        fs::write(
            &cue,
            concat!(
                "FILE \"Disc.bin\" BINARY\n",
                "  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                "  TRACK 02 AUDIO\n    INDEX 01 00:00:23\n",
                "  TRACK 03 AUDIO\n    INDEX 01 00:00:54\n",
            ),
        )
        .unwrap();
        let pcm = (0..89 * 588u32)
            .flat_map(|position| {
                let mut sample = position
                    .wrapping_add_signed(shift)
                    .wrapping_mul(0x9e37_79b9)
                    .wrapping_add(0x1234_5678);
                if damaged && position == 23 * 588 + 777 {
                    sample ^= 0x100;
                }
                sample.to_le_bytes()
            })
            .collect::<Vec<_>>();
        fs::write(cue.with_extension("bin"), pcm).unwrap();
        (dir, cue)
    }

    fn at(checksums: &Checksums, shift: i32) -> u32 {
        checksums
            .v1
            .iter()
            .find(|&(offset, _)| *offset == shift)
            .unwrap()
            .1
    }

    #[test]
    fn matches_libarcstk_disc_id_and_first_middle_last_track_vectors() {
        // Independent libarcstk f5b65f0 generated these values from the same
        // sample stream for each nominal alignment and the altered sample.
        let expected = [
            (
                0,
                false,
                [
                    (0x902998ac, 0x92c23883),
                    (0x7a8266bc, 0x7f75a26a),
                    (0x17b451c8, 0x1c57248e),
                ],
            ),
            (
                7,
                false,
                [
                    (0xa6e18144, 0xa97a245a),
                    (0xec6ec7fa, 0xf1623431),
                    (0x1d6d6f74, 0x22102142),
                ],
            ),
            (
                -11,
                false,
                [
                    (0x2351bd74, 0x25ea6be6),
                    (0xa2ea6036, 0xa7ddc23f),
                    (0xc5916c4c, 0xca344e92),
                ],
            ),
            (
                0,
                true,
                [
                    (0x902998ac, 0x92c23883),
                    (0x7a8570bc, 0x7f78ac6a),
                    (0x17b451c8, 0x1c57248e),
                ],
            ),
        ];
        let (_original, original) = fixture(0, false);
        let original = AudioDisc::load(&original).unwrap();
        for (shift, damaged, tracks) in expected {
            let (_dir, cue) = fixture(shift, damaged);
            let disc = AudioDisc::load(&cue).unwrap();
            for (index, (v1, v2)) in tracks.into_iter().enumerate() {
                let track = index as u32 + 1;
                let (id, checksum) = verify_image_track(&disc, track, || Ok(())).unwrap();
                assert_eq!(
                    id,
                    DiscId {
                        tracks: 3,
                        offsets_sum: 0xa6,
                        weighted_offsets: 0x235,
                        cddb: 0x0600_0103
                    }
                );
                assert_eq!((at(&checksum, 0), checksum.v2), (v1, v2));
                if !damaged {
                    let (_, checksum) = verify_image_track(&original, track, || Ok(())).unwrap();
                    assert_eq!(at(&checksum, shift), v1);
                }
            }
        }
    }

    #[test]
    fn exact_alignment_uses_only_the_selected_track_and_propagates_cancellation() {
        let (_dir, cue) = fixture(0, false);
        let disc = AudioDisc::load(&cue).unwrap();
        let scoring = TrackScoring::new(
            0..31 * FRAMES_PER_SECTOR,
            0..31 * FRAMES_PER_SECTOR,
            false,
            false,
            0,
        )
        .unwrap();
        let end = 54 * FRAMES_PER_SECTOR;
        let middle = disc.pcm(23 * FRAMES_PER_SECTOR..end).unwrap();
        let checksum = scoring.checksums(middle, || Ok(())).unwrap();
        assert_eq!(checksum.v1, [(0, 0x7a8266bc)]);
        assert_eq!(checksum.v2, 0x7f75a26a);
        assert_eq!(
            verify_image_track(&disc, 2, || Err(io::ErrorKind::Interrupted.into()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }
}
