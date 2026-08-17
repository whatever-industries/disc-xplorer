//! Extracting a CD CHD into CUE/BIN.
//!
//! A CHD stores a CD as a flat run of fixed-size frames, and describes the track
//! layout in `CHTR`/`CHT2` metadata strings rather than in the data itself. So
//! extraction is two jobs: read that layout, and copy each track's sectors back
//! out with the padding and subcode stripped.
//!
//! Three details make this more than a copy:
//!
//! 1. Every frame is stored padded to the full 2352 + 96 subcode layout, but a
//!    track's real sector size depends on its mode. A MODE1 track carries 2048
//!    bytes per frame; the rest of the frame is padding that must not reach the
//!    BIN.
//! 2. Each track is padded out to a multiple of four frames inside the CHD, so
//!    track offsets are not the running total of the track lengths. Getting this
//!    wrong misaligns every track after the first.
//! 3. A pregap may or may not be stored. `PGTYPE` marks a stored one with a
//!    leading `V`; without it the gap is implied, and the cue has to say so with
//!    a PREGAP command instead of containing the sectors.
//!
//! The output matches `chdman extractcd`: one BIN with the tracks indexed inside
//! it, plus the cue sheet. Splitting that into per-track files is what the CUE
//! split conversion is for.
//!
//! ## References, and what was actually checked
//!
//! No chdman code is used here. The track table comes from the CHD's own
//! metadata, read through the `chd` crate; the rules for interpreting it are the
//! CD-ROM format MAME defines in `src/lib/util/cdrom.cpp`, which is what chdman
//! itself is built on.
//!
//! - `chd` crate **0.3.4** (see Cargo.lock), features `cd_full`
//! - CHD container format **version 5**
//! - MAME **0.289** (31 July 2026) as the format reference, checked 16 Aug 2026
//!
//! Verified directly: single-track extraction against a real MODE2_RAW CHD, with
//! the result's ISO 9660 listing compared against the same CHD read in place.
//! The four-frame padding rule was confirmed from that file too (257,525 frames
//! stored as 257,528).
//!
//! Not verified against a real file, because none was available: multi-track
//! layouts, and both pregap cases. Those paths are covered by unit tests over
//! synthetic metadata, which pins the arithmetic but not the format reading. If
//! a multi-track CHD turns up, extract it and compare against `chdman extractcd`
//! before trusting the audio track boundaries.
//!
//! Worth re-checking when MAME publishes a new release: whether any track type
//! or metadata field has been added. See TODO.md.

use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::bincue::{ensure_writable, sectors_to_msf, track_filename};
use crate::convert::CANCELLED;

/// Tracks are padded out to a multiple of this many frames inside the CHD.
const TRACK_PADDING: u64 = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct ChdTrack {
    pub number: u32,
    /// The mode as CHD names it: MODE2_RAW, AUDIO, MODE1_RAW…
    pub track_type: String,
    /// Frames stored in the CHD for this track, including a stored pregap.
    pub frames: u64,
    /// Pregap length in frames, stored or implied.
    pub pregap: u64,
    /// True when those pregap frames are actually in the file.
    pub pregap_stored: bool,
    pub postgap: u64,
    /// Frame index in the CHD where this track begins.
    pub chd_frame: u64,
}

/// Bytes of real sector data per frame, and the cue sheet mode that describes
/// it. Modes with no cue equivalent return None rather than being approximated,
/// since a cue naming a mode no tool understands is worse than a clear refusal.
fn track_layout(track_type: &str) -> Option<(u64, &'static str)> {
    match track_type {
        "MODE1" => Some((2048, "MODE1/2048")),
        "MODE1_RAW" => Some((2352, "MODE1/2352")),
        "MODE2" | "MODE2_FORM_MIX" => Some((2336, "MODE2/2336")),
        "MODE2_RAW" => Some((2352, "MODE2/2352")),
        "AUDIO" => Some((2352, "AUDIO")),
        _ => None,
    }
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split_whitespace().find_map(|part| part.strip_prefix(key))
}

/// Read one CHTR or CHT2 metadata string.
///
/// CHTR: `TRACK:n TYPE:t SUBTYPE:s FRAMES:n`
/// CHT2: the same, plus `PREGAP:n PGTYPE:t PGSUB:s POSTGAP:n`
fn parse_track(text: &str) -> Option<ChdTrack> {
    let text = text.trim_end_matches('\0').trim();
    let pgtype = field(text, "PGTYPE:").unwrap_or("");
    Some(ChdTrack {
        number: field(text, "TRACK:")?.parse().ok()?,
        track_type: field(text, "TYPE:")?.to_string(),
        frames: field(text, "FRAMES:")?.parse().ok()?,
        pregap: field(text, "PREGAP:").and_then(|v| v.parse().ok()).unwrap_or(0),
        // MAME marks a pregap whose sectors are really in the file by prefixing
        // its type with V. Without that the gap is implied and the cue has to
        // reconstruct it.
        pregap_stored: pgtype.starts_with('V'),
        postgap: field(text, "POSTGAP:").and_then(|v| v.parse().ok()).unwrap_or(0),
        chd_frame: 0,
    })
}

/// Turn the metadata strings into a track table with CHD frame offsets filled in.
pub fn parse_tracks(entries: &[String]) -> Result<Vec<ChdTrack>, String> {
    let mut tracks: Vec<ChdTrack> = entries
        .iter()
        .filter_map(|t| parse_track(t))
        .collect();
    if tracks.is_empty() {
        return Err("CHD has no CD track metadata (is it a hard disk image?)".to_string());
    }
    tracks.sort_by_key(|t| t.number);

    // Offsets accumulate over the *padded* length of each track, which is what
    // makes this worth computing rather than assuming.
    let mut at = 0u64;
    for track in &mut tracks {
        track.chd_frame = at;
        at += track.frames.div_ceil(TRACK_PADDING) * TRACK_PADDING;
    }

    for track in &tracks {
        if track_layout(&track.track_type).is_none() {
            return Err(format!(
                "Track {} is {}, which has no CUE sheet equivalent",
                track.number, track.track_type
            ));
        }
    }
    Ok(tracks)
}

/// Bytes the extracted BIN will occupy.
pub fn output_size(tracks: &[ChdTrack]) -> u64 {
    tracks
        .iter()
        .map(|t| t.frames * track_layout(&t.track_type).map(|(n, _)| n).unwrap_or(0))
        .sum()
}

/// Every file an extraction will write, so they can be checked up front.
pub fn outputs(stem: &str, tracks: &[ChdTrack], dir: &Path, per_track: bool) -> Vec<PathBuf> {
    if per_track {
        tracks
            .iter()
            .map(|t| dir.join(track_filename(stem, t.number, tracks.len())))
            .collect()
    } else {
        vec![dir.join(format!("{stem}.bin"))]
    }
}

/// The cue sheet describing the extracted BIN, or BINs.
///
/// With one BIN per track every stamp is relative to that track's own file, so
/// a stored pregap sits at 00:00:00 and INDEX 01 follows it. With a single BIN
/// the stamps run across the whole disc.
pub fn cuesheet(basename: &str, tracks: &[ChdTrack], per_track: bool) -> String {
    if per_track {
        let mut out = String::new();
        for track in tracks {
            let (_, mode) = track_layout(&track.track_type).unwrap_or((2352, "AUDIO"));
            let name = track_filename(basename, track.number, tracks.len());
            out.push_str(&format!("FILE \"{name}\" BINARY\r\n"));
            out.push_str(&format!("  TRACK {:02} {}\r\n", track.number, mode));
            if track.pregap > 0 && !track.pregap_stored {
                out.push_str(&format!("    PREGAP {}\r\n", sectors_to_msf(track.pregap)));
                out.push_str("    INDEX 01 00:00:00\r\n");
            } else if track.pregap > 0 {
                out.push_str("    INDEX 00 00:00:00\r\n");
                out.push_str(&format!("    INDEX 01 {}\r\n", sectors_to_msf(track.pregap)));
            } else {
                out.push_str("    INDEX 01 00:00:00\r\n");
            }
            if track.postgap > 0 {
                out.push_str(&format!("    POSTGAP {}\r\n", sectors_to_msf(track.postgap)));
            }
        }
        return out;
    }

    let mut out = format!("FILE \"{basename}.bin\" BINARY\r\n");
    let mut at = 0u64;
    for track in tracks {
        let (_, mode) = track_layout(&track.track_type).unwrap_or((2352, "AUDIO"));
        out.push_str(&format!("  TRACK {:02} {}\r\n", track.number, mode));
        if track.pregap > 0 && !track.pregap_stored {
            // The sectors are not in the BIN, so the cue has to declare the gap
            // rather than point at it.
            out.push_str(&format!("    PREGAP {}\r\n", sectors_to_msf(track.pregap)));
            out.push_str(&format!("    INDEX 01 {}\r\n", sectors_to_msf(at)));
        } else if track.pregap > 0 {
            out.push_str(&format!("    INDEX 00 {}\r\n", sectors_to_msf(at)));
            out.push_str(&format!("    INDEX 01 {}\r\n", sectors_to_msf(at + track.pregap)));
        } else {
            out.push_str(&format!("    INDEX 01 {}\r\n", sectors_to_msf(at)));
        }
        if track.postgap > 0 {
            out.push_str(&format!("    POSTGAP {}\r\n", sectors_to_msf(track.postgap)));
        }
        at += track.frames;
    }
    out
}

/// Copy the tracks out of `src` into a BIN beside `out_cue`, and write the cue.
///
/// `frame_bytes` is the CHD's stored frame size, normally 2448 (a full sector
/// plus its subcode) but 2352 when no subcode was kept.
#[allow(clippy::too_many_arguments)] // Each one is a distinct, unrelated input.
pub fn extract<R: Read + Seek, F: FnMut(u64, u64)>(
    src: &mut R,
    tracks: &[ChdTrack],
    frame_bytes: u64,
    out_cue: &Path,
    per_track: bool,
    overwrite: bool,
    cancel: &Arc<AtomicBool>,
    mut progress: F,
) -> Result<(), String> {
    let stem = out_cue
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Output name is not valid text")?
        .to_string();
    let dir = out_cue.parent().unwrap_or(Path::new("."));
    let written = outputs(&stem, tracks, dir, per_track);
    ensure_writable(&written, overwrite)?;
    let total = output_size(tracks);

    // Read several frames at a time; one frame at a time would seek within the
    // same hunk over and over.
    const BATCH: u64 = 64;
    let mut buf = vec![0u8; (BATCH * frame_bytes) as usize];
    let mut done = 0u64;
    let mut last = 0u64;

    let result = (|| -> Result<(), String> {
        let mut single = if per_track {
            None
        } else {
            Some(BufWriter::with_capacity(
                8 << 20,
                std::fs::File::create(&written[0]).map_err(|e| format!("Create output: {e}"))?,
            ))
        };

        for (n, track) in tracks.iter().enumerate() {
            let (data_bytes, _) = track_layout(&track.track_type)
                .ok_or_else(|| format!("Track {} has an unsupported mode", track.number))?;
            let mut per = if per_track {
                Some(BufWriter::with_capacity(
                    8 << 20,
                    std::fs::File::create(&written[n])
                        .map_err(|e| format!("Create {:?}: {e}", written[n]))?,
                ))
            } else {
                None
            };
            let writer: &mut BufWriter<std::fs::File> =
                per.as_mut().or(single.as_mut()).expect("one writer is always open");

            let mut frame = 0u64;
            while frame < track.frames {
                let count = BATCH.min(track.frames - frame);
                let at = (track.chd_frame + frame) * frame_bytes;
                src.seek(SeekFrom::Start(at)).map_err(|e| format!("Seek: {e}"))?;
                let want = (count * frame_bytes) as usize;
                src.read_exact(&mut buf[..want])
                    .map_err(|e| format!("Read frame {}: {e}", track.chd_frame + frame))?;

                for i in 0..count as usize {
                    let start = i * frame_bytes as usize;
                    writer
                        .write_all(&buf[start..start + data_bytes as usize])
                        .map_err(|e| format!("Write: {e}"))?;
                }
                frame += count;
                done += count * data_bytes;
                if cancel.load(Ordering::SeqCst) {
                    return Err(CANCELLED.to_string());
                }
                if done - last >= total / 100 + 1 {
                    last = done;
                    progress(done, total);
                }
            }
            if let Some(mut w) = per {
                w.flush().map_err(|e| format!("Flush: {e}"))?;
            }
        }
        if let Some(mut w) = single {
            w.flush().map_err(|e| format!("Flush: {e}"))?;
        }
        Ok(())
    })();

    if let Err(e) = result {
        // A half-written set reads as a complete one to anything that trusts
        // the cue, so none of it is left behind.
        for p in &written {
            let _ = std::fs::remove_file(p);
        }
        return Err(e);
    }
    std::fs::write(out_cue, cuesheet(&stem, tracks, per_track))
        .map_err(|e| format!("Write CUE: {e}"))?;
    progress(total, total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn reads_a_single_track_chd() {
        // Verbatim from a real CHD of Crash Team Racing.
        let tracks = parse_tracks(&[meta(
            "TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:257525 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0",
        )])
        .unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track_type, "MODE2_RAW");
        assert_eq!(tracks[0].frames, 257525);
        assert_eq!(tracks[0].chd_frame, 0);
        assert_eq!(output_size(&tracks), 257525 * 2352);
    }

    /// The offset arithmetic is where a multi-track extraction goes wrong, and
    /// it goes wrong silently: every track after the first is shifted, so the
    /// audio plays as noise rather than failing outright.
    #[test]
    fn track_offsets_account_for_the_four_frame_padding() {
        let tracks = parse_tracks(&[
            meta("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:1001 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
            meta("TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:502 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
            meta("TRACK:3 TYPE:AUDIO SUBTYPE:NONE FRAMES:300 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
        ])
        .unwrap();
        // 1001 pads to 1004; 1004 + 504 = 1508.
        assert_eq!(tracks[0].chd_frame, 0);
        assert_eq!(tracks[1].chd_frame, 1004);
        assert_eq!(tracks[2].chd_frame, 1508);
        // The BIN holds the real lengths, not the padded ones.
        assert_eq!(output_size(&tracks), (1001 + 502 + 300) * 2352);
    }

    #[test]
    fn legacy_chtr_metadata_without_pregap_fields_still_parses() {
        let tracks = parse_tracks(&[meta("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:1000")]).unwrap();
        assert_eq!(tracks[0].frames, 1000);
        assert_eq!(tracks[0].pregap, 0);
        assert!(!tracks[0].pregap_stored);
    }

    #[test]
    fn an_implied_pregap_becomes_a_pregap_command() {
        let tracks = parse_tracks(&[
            meta("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:1000 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
            meta("TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:500 PREGAP:150 PGTYPE:AUDIO PGSUB:NONE POSTGAP:0"),
        ])
        .unwrap();
        assert!(!tracks[1].pregap_stored);
        let cue = cuesheet("Disc", &tracks, false);
        assert!(cue.contains("PREGAP 00:02:00"), "{cue}");
        // Track 2 starts right after track 1 in the BIN, gap not included.
        assert!(cue.contains("INDEX 01 00:13:25"), "{cue}");
        assert!(!cue.contains("INDEX 00"), "no stored gap, so no INDEX 00:\n{cue}");
    }

    #[test]
    fn a_stored_pregap_becomes_index_00() {
        let tracks = parse_tracks(&[
            meta("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:1000 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
            meta("TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:650 PREGAP:150 PGTYPE:VAUDIO PGSUB:NONE POSTGAP:0"),
        ])
        .unwrap();
        assert!(tracks[1].pregap_stored);
        let cue = cuesheet("Disc", &tracks, false);
        // Track 2's sectors start at 1000 and its INDEX 01 is 150 further on.
        assert!(cue.contains("INDEX 00 00:13:25"), "{cue}");
        assert!(cue.contains("INDEX 01 00:15:25"), "{cue}");
        assert!(!cue.contains("PREGAP "), "gap is in the file, so no command:\n{cue}");
    }

    #[test]
    fn a_mode_with_no_cue_equivalent_is_refused() {
        let err = parse_tracks(&[meta(
            "TRACK:1 TYPE:MODE2_FORM2 SUBTYPE:NONE FRAMES:100 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0",
        )])
        .unwrap_err();
        assert!(err.contains("MODE2_FORM2"), "{err}");
    }

    /// Copies the right bytes out of the right frames, with subcode dropped.
    #[test]
    fn extraction_strips_padding_and_honours_offsets() {
        let frame_bytes = 2448usize;
        let tracks = parse_tracks(&[
            meta("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:5 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
            meta("TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:3 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
        ])
        .unwrap();
        assert_eq!(tracks[1].chd_frame, 8); // 5 pads to 8

        // Frame n is filled with n in its data area and 0xFF in its subcode, so
        // a wrong offset or a leaked subcode byte is obvious.
        let mut src = vec![0u8; 11 * frame_bytes];
        for f in 0..11usize {
            let at = f * frame_bytes;
            src[at..at + 2352].fill(f as u8);
            src[at + 2352..at + frame_bytes].fill(0xFF);
        }

        let dir = std::env::temp_dir().join("dx_chd_extract");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_cue = dir.join("Disc.cue");
        let cancel = Arc::new(AtomicBool::new(false));
        extract(
            &mut std::io::Cursor::new(src),
            &tracks,
            frame_bytes as u64,
            &out_cue,
            false,
            false,
            &cancel,
            |_, _| {},
        )
        .unwrap();

        let bin = std::fs::read(dir.join("Disc.bin")).unwrap();
        assert_eq!(bin.len(), 8 * 2352);
        assert!(!bin.contains(&0xFF), "subcode leaked into the BIN");
        // Track 1 is frames 0..5, track 2 resumes at frame 8, not frame 5.
        for (i, expect) in [0u8, 1, 2, 3, 4, 8, 9, 10].iter().enumerate() {
            assert_eq!(bin[i * 2352], *expect, "sector {i} came from the wrong frame");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
