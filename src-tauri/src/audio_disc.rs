//! Track boundaries and raw PCM for a complete, single-session CD-DA image.
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

pub(crate) const SECTOR_BYTES: u64 = 2352;
pub(crate) const FRAMES_PER_SECTOR: u64 = 588;

#[derive(Clone)]
pub(crate) struct Toc {
    pub starts: Vec<u64>,
    pub end: u64,
}

pub(crate) struct AudioDisc {
    pub toc: Toc,
    files: Vec<BinFile>,
}

struct BinFile {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

impl BinFile {
    fn check(&self, metadata: &fs::Metadata) -> io::Result<()> {
        if metadata.len() != self.bytes || metadata.modified()? != self.modified {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The audio BIN changed.",
            ));
        }
        Ok(())
    }
}

impl AudioDisc {
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_with_sheet(path).map(|(disc, _)| disc)
    }

    pub fn load_with_sheet(path: &Path) -> Result<(Self, crate::bincue::CueSheet), String> {
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
        {
            return Err("A complete audio BIN/CUE image is required.".into());
        }
        let cue = fs::read_to_string(path).map_err(|e| e.to_string())?;
        for line in cue.lines() {
            let mut words = line.split_whitespace();
            match words
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase()
                .as_str()
            {
                "FILE"
                    if !line
                        .split_whitespace()
                        .last()
                        .is_some_and(|w| w.eq_ignore_ascii_case("BINARY")) =>
                {
                    return Err("Only raw audio BIN files are supported.".into())
                }
                "PREGAP" | "POSTGAP" if words.next() != Some("00:00:00") => {
                    return Err("This CUE contains gaps without stored audio.".into())
                }
                "REM"
                    if words
                        .next()
                        .is_some_and(|w| w.eq_ignore_ascii_case("SESSION"))
                        && words.next().and_then(|w| w.parse::<u32>().ok()) != Some(1) =>
                {
                    return Err("Only single-session audio CDs are supported.".into())
                }
                "INDEX" => {
                    let valid = words.next().and_then(|w| w.parse::<u8>().ok()).is_some_and(|n| n <= 99)
                        && words.next().is_some_and(|w| {
                            let fields: Vec<_> = w.split(':').map(str::parse::<u32>).collect();
                            matches!(fields.as_slice(), [Ok(_), Ok(sec), Ok(frame)] if *sec < 60 && *frame < 75)
                        }) && words.next().is_none();
                    if !valid {
                        return Err("Invalid audio CUE index.".into());
                    }
                }
                _ => {}
            }
        }
        let sheet = crate::bincue::parse_text(path, &cue)?;
        if sheet.blocksize != SECTOR_BYTES {
            return Err("Only raw CD audio is supported.".into());
        }
        let mut files: Vec<BinFile> = Vec::new();
        let mut starts = Vec::new();
        let mut end = 0;
        for file in &sheet.files {
            let canonical = file.path.canonicalize().map_err(|e| e.to_string())?;
            let metadata = fs::metadata(&canonical).map_err(|e| e.to_string())?;
            if file.size % SECTOR_BYTES != 0
                || !metadata.is_file()
                || metadata.len() != file.size
                || files.iter().any(|bin| bin.path == canonical)
            {
                return Err("The CUE has an incomplete or repeated audio BIN.".into());
            }
            let sectors = file.size / SECTOR_BYTES;
            // Bound lead-out before adding indexes or subsequently using byte/sample arithmetic.
            if sectors > 450_000 - end {
                return Err("Invalid CD lead-out.".into());
            }
            for track in &file.tracks {
                if track.mode != "AUDIO"
                    || track.number as usize != starts.len() + 1
                    || track.number > 99
                {
                    return Err("Only complete audio-only CDs are supported.".into());
                }
                let mut ones = track.indexes.iter().filter(|i| i.id == 1);
                let start = ones.next().map(|i| i.lba).ok_or("Missing INDEX 01.")?;
                if ones.next().is_some()
                    || start >= sectors
                    || starts.last().is_some_and(|last| end + start <= *last)
                    || track
                        .indexes
                        .iter()
                        .any(|i| i.lba >= sectors || (i.id == 0 && i.lba > start))
                    || track
                        .indexes
                        .windows(2)
                        .any(|pair| pair[0].id >= pair[1].id || pair[0].lba > pair[1].lba)
                {
                    return Err("Invalid audio track boundaries.".into());
                }
                starts.push(end + start);
            }
            let modified = metadata.modified().map_err(|e| e.to_string())?;
            files.push(BinFile {
                path: canonical,
                bytes: file.size,
                modified,
            });
            end += sectors;
        }
        if starts.is_empty() || end <= *starts.last().unwrap() {
            return Err("Invalid CD lead-out.".into());
        }
        Ok((Self {
            toc: Toc { starts, end },
            files,
        }, sheet))
    }

    pub fn frames(&self) -> u64 {
        (self.toc.end - self.toc.starts[0]) * FRAMES_PER_SECTOR
    }

    pub fn check_files(&self) -> io::Result<()> {
        for bin in &self.files {
            bin.check(&fs::metadata(&bin.path)?)?;
        }
        Ok(())
    }

    pub fn pcm(&self, frames: std::ops::Range<u64>) -> io::Result<Pcm> {
        if frames.start > frames.end || frames.end > self.frames() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid CD sample range.",
            ));
        }
        self.pcm_at(self.toc.starts[0] * SECTOR_BYTES + frames.start * 4, (frames.end - frames.start) * 4)
    }

    pub fn full_pcm(&self) -> io::Result<(Pcm, u64)> {
        let bytes = self.toc.end * SECTOR_BYTES;
        self.pcm_at(0, bytes).map(|pcm| (pcm, bytes))
    }

    fn pcm_at(&self, mut start: u64, mut remain: u64) -> io::Result<Pcm> {
        let mut parts = Vec::new();
        for bin in &self.files {
            if start >= bin.bytes {
                start -= bin.bytes;
                continue;
            }
            let len = remain.min(bin.bytes - start);
            if len == 0 {
                break;
            }
            let mut file = File::open(&bin.path)?;
            bin.check(&file.metadata()?)?;
            file.seek(SeekFrom::Start(start))?;
            parts.push(file.take(len));
            remain -= len;
            start = 0;
        }
        Ok(Pcm(parts.into_iter().peekable()))
    }
}

pub(crate) struct Pcm(std::iter::Peekable<std::vec::IntoIter<std::io::Take<File>>>);

impl Read for Pcm {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while let Some(part) = self.0.peek_mut() {
            if part.limit() == 0 {
                self.0.next();
                continue;
            }
            let n = part.read(buf)?;
            if n == 0 && !buf.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "The audio BIN ended early.",
                ));
            }
            return Ok(n);
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_and_shared_images_preserve_real_gaps_and_exclude_hidden_audio() {
        let temp = crate::audio_export::TempDir::create(&std::env::temp_dir()).unwrap();
        let dir = &temp.0;
        let mut expected = Vec::new();
        for (name, sectors) in [("one.bin", &[1, 2, 3][..]), ("two.bin", &[4, 5, 6][..])] {
            let pcm: Vec<_> = sectors
                .iter()
                .flat_map(|&n| std::iter::repeat_n(n, 2352))
                .collect();
            fs::write(dir.join(name), &pcm).unwrap();
            expected.extend_from_slice(&pcm);
        }
        fs::write(dir.join("shared.bin"), &expected).unwrap();
        for cue in [
            "FILE \"one.bin\" BINARY\n TRACK 01 AUDIO\n INDEX 00 00:00:00\n INDEX 01 00:00:01\nFILE \"two.bin\" BINARY\n TRACK 02 AUDIO\n INDEX 00 00:00:00\n INDEX 01 00:00:01\n",
            "FILE \"shared.bin\" BINARY\n TRACK 01 AUDIO\n INDEX 00 00:00:00\n INDEX 01 00:00:01\n TRACK 02 AUDIO\n INDEX 00 00:00:03\n INDEX 01 00:00:04\n",
        ] {
            let path = dir.join("disc.cue");
            fs::write(&path, cue).unwrap();
            let disc = AudioDisc::load(&path).unwrap();
            assert_eq!(disc.toc.starts, vec![1, 4]);
            let mut pcm = Vec::new();
            disc.pcm(0..disc.frames()).unwrap().read_to_end(&mut pcm).unwrap();
            assert_eq!(pcm, expected[2352..]);
            fs::write(&path, cue.replace("INDEX 01 00:00:01", "INDEX 01 invalid")).unwrap();
            assert!(AudioDisc::load(&path).is_err());
        }
    }
}
