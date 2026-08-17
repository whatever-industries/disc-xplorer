//! Merging and splitting CUE/BIN track layouts.
//!
//! The same disc is dumped two ways in the wild: one BIN per track, which is
//! what Redump publishes, and a single BIN with the tracks indexed inside it,
//! which is what most emulators and virtual drives want. Converting between
//! them is not a data change at all, only a repackaging, but it does mean
//! rewriting the cue sheet so that every INDEX stamp still points at the right
//! sector.
//!
//! Ported from binmerge by Chris Putnam (GPL-2.0-or-later), which works out the
//! sector arithmetic and the Redump file-naming rules this follows. See the
//! third-party references in README.md. Verified against it directly: merging
//! real PS1, PC Engine and multi-session dumps produces a byte-identical BIN
//! and the same FILE/TRACK/INDEX lines.
//!
//! One deliberate difference: binmerge keeps only FILE, TRACK and INDEX lines,
//! and its README notes that Redump cue sheets carry information it "cannot
//! reasonably preserve". The lines it drops (REM, FLAGS, ISRC, PREGAP, POSTGAP
//! and the CD-TEXT fields) say nothing about where tracks sit in a file, so
//! nothing stops them being carried across. This keeps them.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::convert::CANCELLED;

const CHUNK: usize = 4 << 20;

#[derive(Clone)]
pub struct CueIndex {
    pub id: u32,
    /// Sectors from the start of this track's FILE.
    pub lba: u64,
}

#[derive(Clone)]
pub struct CueTrack {
    pub number: u32,
    /// The mode as written: AUDIO, MODE1/2352, MODE2/2352, CDI/2336, CDG…
    pub mode: String,
    pub indexes: Vec<CueIndex>,
    /// Track attributes written before the INDEX lines: FLAGS, ISRC, PREGAP,
    /// POSTGAP, TITLE, PERFORMER and so on.
    pub pre: Vec<String>,
    /// Lines written after this track's last INDEX, which on a multi-session
    /// sheet are the session markers introducing whatever comes next: REM
    /// LEAD-OUT, REM SESSION 02, REM LEAD-IN. They are kept separate because
    /// re-emitting them with the attributes would move them above the INDEX
    /// lines and change which track they appear to describe.
    pub post: Vec<String>,
    /// Length in sectors. Known from the cue for every track but the last in a
    /// file, where it comes from the file size instead.
    pub sectors: u64,
}

pub struct CueFile {
    pub path: PathBuf,
    pub size: u64,
    pub tracks: Vec<CueTrack>,
}

pub struct CueSheet {
    /// CATALOG, CDTEXTFILE and any leading REM lines, before the first FILE.
    pub header: Vec<String>,
    pub files: Vec<CueFile>,
    pub blocksize: u64,
}

impl CueSheet {
    pub fn track_count(&self) -> usize {
        self.files.iter().map(|f| f.tracks.len()).sum()
    }
    /// Total audio/data bytes across every BIN the sheet refers to.
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Sector size for a track mode. A disc cannot mix these, so the first track
/// that names a known mode fixes it for the whole sheet.
fn blocksize_for(mode: &str) -> Option<u64> {
    match mode {
        "AUDIO" | "MODE1/2352" | "MODE2/2352" | "CDI/2352" => Some(2352),
        "CDG" => Some(2448),
        "MODE1/2048" => Some(2048),
        "MODE2/2336" | "CDI/2336" => Some(2336),
        _ => None,
    }
}

pub fn sectors_to_msf(sectors: u64) -> String {
    let minutes = sectors / 4500;
    let seconds = (sectors % 4500) / 75;
    let frames = sectors % 75;
    format!("{minutes:02}:{seconds:02}:{frames:02}")
}

pub fn msf_to_sectors(stamp: &str) -> Option<u64> {
    let mut parts = stamp.split(':');
    let m: u64 = parts.next()?.trim().parse().ok()?;
    let s: u64 = parts.next()?.trim().parse().ok()?;
    let f: u64 = parts.next()?.trim().parse().ok()?;
    Some(m * 60 * 75 + s * 75 + f)
}

/// The name Redump gives a track's BIN.
///
/// The convention is inconsistent and this reproduces it exactly rather than
/// tidying it up, because the point of these names is to match the DAT: a
/// single-track disc has no suffix at all, fewer than ten tracks are numbered
/// without a leading zero, and ten or more are zero-padded.
pub fn track_filename(prefix: &str, track_num: u32, track_count: usize) -> String {
    if track_count == 1 {
        format!("{prefix}.bin")
    } else if track_count > 9 {
        format!("{prefix} (Track {track_num:02}).bin")
    } else {
        format!("{prefix} (Track {track_num}).bin")
    }
}

fn quoted(line: &str) -> Option<&str> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Read a cue sheet and everything needed to repackage it.
pub fn parse(cue_path: &Path) -> Result<CueSheet, String> {
    let text = std::fs::read_to_string(cue_path).map_err(|e| format!("Cannot read CUE: {e}"))?;
    let dir = cue_path.parent().unwrap_or(Path::new("."));

    let mut header: Vec<String> = Vec::new();
    let mut files: Vec<CueFile> = Vec::new();
    let mut blocksize: Option<u64> = None;
    let mut missing: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let upper = trimmed.to_uppercase();

        if upper.starts_with("FILE ") {
            let name = quoted(trimmed)
                .ok_or_else(|| format!("Malformed FILE line: {trimmed}"))?;
            // Cue sheets from other systems use backslashes.
            let rel = name.replace('\\', "/");
            let path = dir.join(&rel);
            let size = match std::fs::metadata(&path) {
                Ok(m) => m.len(),
                Err(_) => {
                    missing.push(rel);
                    continue;
                }
            };
            files.push(CueFile { path, size, tracks: Vec::new() });
        } else if upper.starts_with("TRACK ") {
            let Some(file) = files.last_mut() else { continue };
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            let number = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let mode = parts.get(2).map(|s| s.to_uppercase()).unwrap_or_default();
            if blocksize.is_none() {
                blocksize = blocksize_for(&mode);
            }
            file.tracks.push(CueTrack { number, mode, indexes: Vec::new(), pre: Vec::new(), post: Vec::new(), sectors: 0 });
        } else if upper.starts_with("INDEX ") {
            let Some(track) = files.last_mut().and_then(|f| f.tracks.last_mut()) else { continue };
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            let id = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let lba = parts.get(2).and_then(|s| msf_to_sectors(s)).unwrap_or(0);
            track.indexes.push(CueIndex { id, lba });
        } else if let Some(track) = files.last_mut().and_then(|f| f.tracks.last_mut()) {
            // Anything else inside a track: keep it as written, on the side of
            // the INDEX lines it was written on.
            if track.indexes.is_empty() {
                track.pre.push(trimmed.to_string());
            } else {
                track.post.push(trimmed.to_string());
            }
        } else {
            header.push(trimmed.to_string());
        }
    }

    if !missing.is_empty() {
        return Err(format!("Missing BIN file(s): {}", missing.join(", ")));
    }
    if files.is_empty() {
        return Err("No BIN files listed in the CUE".to_string());
    }
    let blocksize = blocksize.ok_or("CUE lists no track mode this understands")?;
    if files.iter().any(|f| f.tracks.is_empty()) {
        return Err("CUE has a FILE with no tracks".to_string());
    }

    let mut sheet = CueSheet { header, files, blocksize };
    fill_sector_counts(&mut sheet);
    Ok(sheet)
}

/// Work out how long each track is.
///
/// Within a file, a track runs to the start of the next one. The last track in
/// a file runs to the end of that file. Note this measures from each track's
/// *first* index, so a pregap counts toward the track it precedes, which is
/// where Redump puts it in a split set.
fn fill_sector_counts(sheet: &mut CueSheet) {
    let bs = sheet.blocksize;
    for file in &mut sheet.files {
        let mut end = file.size / bs;
        for track in file.tracks.iter_mut().rev() {
            let start = track.indexes.first().map(|i| i.lba).unwrap_or(0);
            track.sectors = end.saturating_sub(start);
            end = start;
        }
    }
}

fn render_track(out: &mut String, track: &CueTrack, index_lba: impl Fn(&CueIndex) -> u64) {
    out.push_str(&format!("  TRACK {:02} {}\r\n", track.number, track.mode));
    for line in &track.pre {
        out.push_str(&format!("    {line}\r\n"));
    }
    for index in &track.indexes {
        out.push_str(&format!(
            "    INDEX {:02} {}\r\n",
            index.id,
            sectors_to_msf(index_lba(index))
        ));
    }
    // Session markers sit at sheet level, not indented under the track.
    for line in &track.post {
        out.push_str(&format!("{line}\r\n"));
    }
}

/// Cue sheet for a single BIN holding every track.
///
/// Each file's tracks shift by however many sectors the files before it took
/// up, since concatenating the BINs is what moves them.
pub fn merged_cuesheet(basename: &str, sheet: &CueSheet) -> String {
    let mut out = String::new();
    for line in &sheet.header {
        out.push_str(&format!("{line}\r\n"));
    }
    out.push_str(&format!("FILE \"{basename}.bin\" BINARY\r\n"));
    let mut offset = 0u64;
    for file in &sheet.files {
        for track in &file.tracks {
            render_track(&mut out, track, |i| offset + i.lba);
        }
        offset += file.size / sheet.blocksize;
    }
    out
}

/// Cue sheet for one BIN per track.
///
/// Every track's stamps become relative to its own file, so they are shifted
/// back by the track's first index.
pub fn split_cuesheet(basename: &str, sheet: &CueSheet) -> String {
    let mut out = String::new();
    for line in &sheet.header {
        out.push_str(&format!("{line}\r\n"));
    }
    let count = sheet.track_count();
    for file in &sheet.files {
        for track in &file.tracks {
            let base = track.indexes.first().map(|i| i.lba).unwrap_or(0);
            let name = track_filename(basename, track.number, count);
            out.push_str(&format!("FILE \"{name}\" BINARY\r\n"));
            render_track(&mut out, track, |i| i.lba.saturating_sub(base));
        }
    }
    out
}

/// Refuse to start if the run would replace a file nobody agreed to replace.
///
/// The batch planner resolves conflicts on the cue sheet's own path, because
/// that is the output it names. But a repackaging writes BINs beside it whose
/// names the planner never saw, so "rename" or "skip" can still land on top of
/// an existing BIN from an interrupted earlier run. Truncating one silently is
/// the worst outcome available: the cue looks right and the data is gone.
pub fn ensure_writable(paths: &[PathBuf], overwrite: bool) -> Result<(), String> {
    if overwrite {
        return Ok(());
    }
    if let Some(clash) = paths.iter().find(|p| p.exists()) {
        return Err(format!(
            "{} already exists. Choose Overwrite, or a different output folder.",
            clash.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ));
    }
    Ok(())
}

/// Copy `len` bytes from `src` into `dst`, or to the end of `src` when `len` is
/// None. Returns bytes written.
fn copy_span(
    src: &mut File,
    dst: &mut BufWriter<File>,
    len: Option<u64>,
    cancel: &Arc<AtomicBool>,
    done: &mut u64,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let mut buf = vec![0u8; CHUNK];
    let mut left = len.unwrap_or(u64::MAX);
    let mut last = *done;
    while left > 0 {
        let want = (left.min(CHUNK as u64)) as usize;
        let n = src.read(&mut buf[..want]).map_err(|e| format!("Read: {e}"))?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).map_err(|e| format!("Write: {e}"))?;
        left -= n as u64;
        *done += n as u64;
        if cancel.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        if *done - last >= total / 100 + 1 {
            last = *done;
            progress(*done, total);
        }
    }
    if let Some(len) = len {
        if left > 0 {
            return Err(format!("BIN ended {left} bytes short of what the CUE describes"));
        }
        let _ = len;
    }
    Ok(())
}

/// Write every BIN end to end as one file, and the cue sheet that indexes it.
///
/// `out_cue` fixes both names: the BIN is written beside it with the same stem,
/// which is what keeps a renamed output self-consistent.
pub fn merge<F: FnMut(u64, u64)>(
    sheet: &CueSheet,
    out_cue: &Path,
    overwrite: bool,
    cancel: &Arc<AtomicBool>,
    mut progress: F,
) -> Result<(), String> {
    let stem = out_cue
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Output name is not valid text")?
        .to_string();
    let out_bin = out_cue.with_extension("bin");
    ensure_writable(std::slice::from_ref(&out_bin), overwrite)?;

    let total = sheet.total_bytes();
    let mut writer = BufWriter::with_capacity(
        CHUNK,
        File::create(&out_bin).map_err(|e| format!("Create output: {e}"))?,
    );
    let mut done = 0u64;
    for file in &sheet.files {
        let mut src = File::open(&file.path).map_err(|e| format!("Open {:?}: {e}", file.path))?;
        if let Err(e) = copy_span(&mut src, &mut writer, None, cancel, &mut done, total, &mut progress) {
            drop(writer);
            let _ = std::fs::remove_file(&out_bin);
            return Err(e);
        }
    }
    writer.flush().map_err(|e| format!("Flush: {e}"))?;
    drop(writer);

    std::fs::write(out_cue, merged_cuesheet(&stem, sheet))
        .map_err(|e| format!("Write CUE: {e}"))?;
    progress(total, total);
    Ok(())
}

/// Write one BIN per track, and the cue sheet that names them.
pub fn split<F: FnMut(u64, u64)>(
    sheet: &CueSheet,
    out_cue: &Path,
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
    let count = sheet.track_count();
    let total = sheet.total_bytes();

    // Every BIN is checked before the first one is created, so a clash does not
    // leave half a set behind.
    let targets: Vec<PathBuf> = sheet
        .files
        .iter()
        .flat_map(|f| f.tracks.iter())
        .map(|t| dir.join(track_filename(&stem, t.number, count)))
        .collect();
    ensure_writable(&targets, overwrite)?;

    let mut written: Vec<PathBuf> = Vec::new();
    let mut done = 0u64;
    // Splitting a partially merged sheet is a real case, so this walks every
    // file rather than assuming a single one the way binmerge does.
    for file in &sheet.files {
        let mut src = File::open(&file.path).map_err(|e| format!("Open {:?}: {e}", file.path))?;
        for track in &file.tracks {
            let start = track.indexes.first().map(|i| i.lba).unwrap_or(0) * sheet.blocksize;
            src.seek(SeekFrom::Start(start)).map_err(|e| format!("Seek: {e}"))?;

            let out_path = dir.join(track_filename(&stem, track.number, count));
            let result = File::create(&out_path)
                .map_err(|e| format!("Create {out_path:?}: {e}"))
                .and_then(|f| {
                    let mut dst = BufWriter::with_capacity(CHUNK, f);
                    copy_span(
                        &mut src,
                        &mut dst,
                        Some(track.sectors * sheet.blocksize),
                        cancel,
                        &mut done,
                        total,
                        &mut progress,
                    )?;
                    dst.flush().map_err(|e| format!("Flush: {e}"))
                });
            written.push(out_path);
            if let Err(e) = result {
                // A half-written set is worse than none: it looks like a
                // complete dump to anything that only checks the cue.
                for p in &written {
                    let _ = std::fs::remove_file(p);
                }
                return Err(e);
            }
        }
    }

    std::fs::write(out_cue, split_cuesheet(&stem, sheet))
        .map_err(|e| format!("Write CUE: {e}"))?;
    progress(total, total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("dx_bincue").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A three-track disc: a data track, then two audio tracks each with a
    /// 150-sector pregap, which is the shape of most PSX dumps.
    fn write_split_set(dir: &Path) -> PathBuf {
        let sizes = [(1u32, "MODE2/2352", 600u64), (2, "AUDIO", 450), (3, "AUDIO", 300)];
        let mut cue = String::from("REM COMMENT \"kept\"\r\n");
        for (num, mode, sectors) in sizes {
            let name = track_filename("Disc", num, 3);
            // Fill each track with a recognisable byte so a misplaced boundary
            // shows up as a wrong value rather than as plausible noise.
            std::fs::write(dir.join(&name), vec![num as u8; (sectors * 2352) as usize]).unwrap();
            cue.push_str(&format!("FILE \"{name}\" BINARY\r\n  TRACK {num:02} {mode}\r\n"));
            if num == 1 {
                cue.push_str("    INDEX 01 00:00:00\r\n");
            } else {
                cue.push_str("    FLAGS DCP\r\n    INDEX 00 00:00:00\r\n    INDEX 01 00:02:00\r\n");
            }
        }
        let path = dir.join("Disc.cue");
        std::fs::write(&path, cue).unwrap();
        path
    }

    #[test]
    fn parses_a_split_set_and_measures_each_track() {
        let dir = scratch("parse");
        let sheet = parse(&write_split_set(&dir)).unwrap();
        assert_eq!(sheet.blocksize, 2352);
        assert_eq!(sheet.files.len(), 3);
        assert_eq!(sheet.track_count(), 3);
        let sectors: Vec<u64> = sheet.files.iter().map(|f| f.tracks[0].sectors).collect();
        assert_eq!(sectors, vec![600, 450, 300]);
    }

    #[test]
    fn merged_stamps_are_offset_by_the_files_before_them() {
        let dir = scratch("merge_cue");
        let sheet = parse(&write_split_set(&dir)).unwrap();
        let cue = merged_cuesheet("Disc (Merged)", &sheet);

        assert!(cue.contains("FILE \"Disc (Merged).bin\" BINARY"));
        // Track 2's file starts 600 sectors in: INDEX 00 lands there, and its
        // INDEX 01 sits 150 sectors further on, at 750 = 00:10:00.
        assert!(cue.contains("INDEX 00 00:08:00"), "{cue}");
        assert!(cue.contains("INDEX 01 00:10:00"), "{cue}");
        // Track 3 starts at 1050 (00:14:00), INDEX 01 at 1200 (00:16:00).
        assert!(cue.contains("INDEX 00 00:14:00"), "{cue}");
        assert!(cue.contains("INDEX 01 00:16:00"), "{cue}");
        // The lines binmerge drops.
        assert!(cue.contains("REM COMMENT \"kept\""), "{cue}");
        assert!(cue.contains("FLAGS DCP"), "{cue}");
        // A track attribute belongs above the INDEX lines it qualifies.
        let flags = cue.find("FLAGS DCP").unwrap();
        let first_index = cue.find("INDEX 00 00:08:00").unwrap();
        assert!(flags < first_index, "FLAGS should precede the track's indexes");
    }

    #[test]
    fn a_split_set_survives_a_round_trip_through_merge() {
        let dir = scratch("round_trip");
        let sheet = parse(&write_split_set(&dir)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));

        let merged_cue = dir.join("Merged.cue");
        merge(&sheet, &merged_cue, false, &cancel, |_, _| {}).unwrap();
        let merged_bin = dir.join("Merged.bin");
        assert_eq!(
            std::fs::metadata(&merged_bin).unwrap().len(),
            (600 + 450 + 300) * 2352
        );

        // Split it back out and check every track is byte-identical.
        let back = parse(&merged_cue).unwrap();
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.track_count(), 3);
        let out = scratch("round_trip_out");
        split(&back, &out.join("Disc.cue"), false, &cancel, |_, _| {}).unwrap();

        for num in 1..=3u32 {
            let name = track_filename("Disc", num, 3);
            let before = std::fs::read(dir.join(&name)).unwrap();
            let after = std::fs::read(out.join(&name)).unwrap();
            assert_eq!(before.len(), after.len(), "track {num} changed length");
            assert!(before == after, "track {num} differs");
        }
        // And the regenerated cue matches the one we started from.
        let split_cue = std::fs::read_to_string(out.join("Disc.cue")).unwrap();
        assert!(split_cue.contains("FILE \"Disc (Track 2).bin\" BINARY"), "{split_cue}");
        assert!(split_cue.contains("INDEX 01 00:02:00"), "{split_cue}");
    }

    #[test]
    fn an_existing_bin_is_not_quietly_replaced() {
        let dir = scratch("clobber");
        let sheet = parse(&write_split_set(&dir)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));

        // A merge writing "Disc.bin" would land on the track 1 file of the split
        // set if the naming ever collided; more realistically, on the output of
        // an interrupted earlier run.
        let out = scratch("clobber_out");
        std::fs::write(out.join("Merged.bin"), b"precious").unwrap();
        let err = merge(&sheet, &out.join("Merged.cue"), false, &cancel, |_, _| {}).unwrap_err();
        assert!(err.contains("Merged.bin"), "{err}");
        assert_eq!(std::fs::read(out.join("Merged.bin")).unwrap(), b"precious");

        // And it goes ahead when overwriting was actually asked for.
        merge(&sheet, &out.join("Merged.cue"), true, &cancel, |_, _| {}).unwrap();
        assert_eq!(
            std::fs::metadata(out.join("Merged.bin")).unwrap().len(),
            (600 + 450 + 300) * 2352
        );
    }

    #[test]
    fn a_split_checks_every_track_before_writing_any() {
        let dir = scratch("clobber_split");
        let sheet = parse(&write_split_set(&dir)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let out = scratch("clobber_split_out");

        // The clash is on the last track, so a check that only looked at the
        // first would already have written two files by the time it noticed.
        std::fs::write(out.join("Disc (Track 3).bin"), b"precious").unwrap();
        let err = split(&sheet, &out.join("Disc.cue"), false, &cancel, |_, _| {}).unwrap_err();
        assert!(err.contains("Disc (Track 3).bin"), "{err}");
        assert!(!out.join("Disc (Track 1).bin").exists(), "nothing should have been written");
        assert_eq!(std::fs::read(out.join("Disc (Track 3).bin")).unwrap(), b"precious");
    }

    #[test]
    fn redump_track_naming_matches_the_convention() {
        assert_eq!(track_filename("Game", 1, 1), "Game.bin");
        assert_eq!(track_filename("Game", 2, 3), "Game (Track 2).bin");
        assert_eq!(track_filename("Game", 2, 12), "Game (Track 02).bin");
        assert_eq!(track_filename("Game", 11, 12), "Game (Track 11).bin");
    }

    #[test]
    fn msf_round_trips() {
        for s in [0u64, 74, 75, 150, 4499, 4500, 123_456] {
            assert_eq!(msf_to_sectors(&sectors_to_msf(s)), Some(s));
        }
    }

    #[test]
    fn a_missing_bin_is_reported_rather_than_guessed_at() {
        let dir = scratch("missing");
        std::fs::write(
            dir.join("Bad.cue"),
            "FILE \"nope.bin\" BINARY\r\n  TRACK 01 AUDIO\r\n    INDEX 01 00:00:00\r\n",
        )
        .unwrap();
        let Err(err) = parse(&dir.join("Bad.cue")) else { panic!("should not parse") };
        assert!(err.contains("nope.bin"), "{err}");
    }
}
