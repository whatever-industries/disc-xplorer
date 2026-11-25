//! A companion text report, generated before dump logs are archived.
use super::*;
use std::fmt::Write as _;

pub(crate) fn is_report_name(filename: &str, name: &str) -> bool {
    let base = format!("{name}_disc-xplorer");
    if filename == format!("{base}.txt") { return true; }
    filename.strip_prefix(&format!("{base}."))
        .and_then(|s| s.strip_suffix(".txt"))
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok_and(|n| n >= 2))
}

fn line(out: &mut String, label: &str, value: &str) {
    let value = if value.is_empty() { "Not recorded" } else { value };
    // Disc metadata can contain line breaks; keep each field on its own line.
    let value: String = value.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let _ = writeln!(out, "{label}: {value}");
}

fn render(image: &Path, info: &disc_info::DiscInfo, dates: &DateReport) -> String {
    let mut out = String::from("Disc Xplorer Report\n===================\n");
    line(&mut out, "Disc Xplorer version", env!("CARGO_PKG_VERSION"));
    line(&mut out, "Report generated (UTC)", &time::OffsetDateTime::now_utc().to_string());
    line(&mut out, "Image", &image.file_name().unwrap_or_default().to_string_lossy());
    line(&mut out, "Format", &ext_of(image).to_uppercase());
    line(&mut out, "Filesystems", &info.filesystems.join(", "));
    out.push_str("\nDates & Times\n-------------\n");
    line(&mut out, "Absolute latest date", &dates.absolute_latest_date);
    line(&mut out, "Sources", &dates.absolute_latest_sources.join(", "));
    line(&mut out, "Latest file date", &dates.latest_file_date);
    line(&mut out, "File path", &dates.latest_file_path);
    line(&mut out, "Latest directory date", &dates.latest_directory_date);
    line(&mut out, "Directory path", &dates.latest_directory_path);
    line(&mut out, "PVD creation date", &dates.pvd_created);
    line(&mut out, "PVD modification date", &dates.pvd_modified);
    line(&mut out, "Entries scanned", &dates.entries_scanned.to_string());
    out.push_str("\nFile and directory dates are recorded local times; their timezone offsets are unavailable in filesystem listings.\nPVD dates retain recorded offsets. Absolute latest date compares displayed calendar times, not UTC instants.\nIt is the latest recorded timestamp found, not a verified mastering or release date. '/' is the root directory.\n");
    out.push_str("\nDisc Info\n---------\n");
    for section in &info.sections {
        line(&mut out, "Section", &section.title);
        for field in &section.fields { line(&mut out, &field.label, &field.value); }
        out.push('\n');
    }
    if info.sections.is_empty() { out.push_str("Header details are not available for this image.\n"); }
    out.push_str("\nTracks\n------\n");
    for track in &info.tracks {
        let _ = writeln!(out, "Track {} | {} | Session {} | Start LBA {} | Sectors {} | Pregap {}",
            track.number, track.mode, track.session, track.start_lba, track.num_sectors, track.pregap_sectors);
    }
    if info.tracks.is_empty() { out.push_str("No track layout available.\n"); }
    out
}

#[derive(Default)]
pub(crate) struct AudioReport {
    pub rip: Option<(String, crate::disc_dump::assessment::SplitAssessment)>,
    pub ctdb: Option<crate::ctdb::Report>,
}

fn audio_tracks(
    text: &mut String,
    image_path: String,
    drive: &str,
    check_result: &crate::disc_dump::assessment::SplitAssessment,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let tracks = get_cue_tracks(image_path)?;
    text.push_str("\nAudio Rip\n---------\n");
    line(text, "Drive", drive);
    line(text, "Drive read offset (stereo samples)", &format_offset(check_result.audio_read_offset()));
    line(text, "Disc write offset (stereo samples)", &format_offset(check_result.write_offset));
    text.push_str("PCM CRC32 covers INDEX 01 to the next track boundary or lead-out, as stored.\n");
    let mut buf = vec![0u8; 256 * 1024];
    for (idx, track) in tracks.iter().enumerate().filter(|(_, t)| !t.is_data && t.mode == "AUDIO") {
        check()?;
        let (status, counts) = check_result.track_audio_result(track.number);
        let _ = write!(text, "Track {:02} | {status}", track.number);
        if let Some((skip, c2)) = counts {
            let _ = write!(text, " | Skipped sectors {skip} | C2 sectors {c2}");
        }
        match open_audio_src(&tracks, idx, GapMode::AppendPrevious) {
            Ok((mut input, length)) => {
                let mut hash = crc32fast::Hasher::new();
                let mut left = length;
                while left > 0 {
                    check()?;
                    let n = left.min(buf.len() as u64) as usize;
                    if input.read_exact(&mut buf[..n]).is_err() { break; }
                    hash.update(&buf[..n]);
                    left -= n as u64;
                }
                let _ = write!(text, " | Duration {} | {} stereo samples", crate::bincue::sectors_to_msf(length / 2352), length / 4);
                if left == 0 && length > 0 { let _ = write!(text, " | PCM CRC32 {:08x}", hash.finalize()); }
                else { text.push_str(" | PCM CRC32 unavailable"); }
            }
            Err(_) => text.push_str(" | PCM CRC32 unavailable"),
        }
        text.push('\n');
    }
    Ok(())
}

fn format_offset(offset: Option<i64>) -> String {
    offset.map(|value| format!("{value:+}")).unwrap_or_default()
}

pub(crate) fn write_single_audio_report(
    destination: &Path, drive: &str, track: u8, read_offset: i64, refined: bool, bytes: u64, crc: u32,
) -> Result<PathBuf, String> {
    let name = destination.file_stem().and_then(|s| s.to_str()).ok_or("Invalid audio filename.")?;
    let output = destination.parent().ok_or("Invalid audio output folder.")?;
    let mut text = String::from("Disc Xplorer Audio Rip\n======================\n");
    line(&mut text, "Drive", drive);
    line(&mut text, "Drive read offset (stereo samples)", &format_offset(Some(read_offset)));
    let result = if refined { "Verified after recovery" } else { "No unresolved errors reported" };
    let _ = writeln!(text, "Track {track:02} | {result} | Duration {} | {} stereo samples | PCM CRC32 {crc:08x}",
        crate::bincue::sectors_to_msf(bytes / 2352), bytes / 4);
    write_report(output, name, &text)
}

pub(crate) fn write_dump_report(
    image: &Path, output: &Path, name: &str, audio: AudioReport, mut check: impl FnMut() -> Result<(), String>,
) -> Result<PathBuf, String> {
    check()?;
    let image_path = image.to_string_lossy().into_owned();
    // An auxiliary metadata error must not suppress available rip evidence.
    let mut text = match disc_info::collect_info(&image_path)
        .and_then(|info| { check()?; disc_date_report(image_path.clone(), None).map(|dates| render(image, &info, &dates)) })
    {
        Ok(text) => text,
        Err(error) if audio.rip.is_some() || audio.ctdb.is_some() => {
            let mut text = String::from("Disc Xplorer Report\n===================\n");
            line(&mut text, "Disc details unavailable", &error);
            text
        }
        Err(error) => return Err(error),
    };
    if let Some((drive, evidence)) = audio.rip {
        if let Err(error) = audio_tracks(&mut text, image_path, &drive, &evidence, &mut check) {
            check()?;
            line(&mut text, "Audio rip details unavailable", &error);
        }
    }
    if let Some(report) = audio.ctdb {
        text.push('\n');
        text.push_str(&report.text());
    }
    check()?;
    write_report(output, name, &text)
}

fn write_report(output: &Path, name: &str, text: &str) -> Result<PathBuf, String> {
    // Never replace an existing report (or follow a pre-existing symlink).
    // Refinement can retain earlier reports as numbered snapshots.
    for index in 1..=1000 {
        let suffix = if index == 1 { String::new() } else { format!(".{index}") };
        let path = output.join(format!("{name}_disc-xplorer{suffix}.txt"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let result = file.write_all(text.as_bytes()).and_then(|_| file.flush());
                if let Err(error) = result { drop(file); let _ = fs::remove_file(&path); return Err(error.to_string()); }
                return Ok(path);
            },
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Too many existing Disc Xplorer reports.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_keeps_per_track_results_separate_from_offsets_and_pcm() {
        let temp = crate::audio_export::TempDir::create(&std::env::temp_dir()).unwrap();
        let image = temp.0.join("Disc.cue");
        fs::write(temp.0.join("Audio.bin"), vec![0; 3 * 2352]).unwrap();
        fs::write(&image, "FILE \"Audio.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:00:01\n").unwrap();
        let mut split = crate::disc_dump::assessment::SplitAssessment::default();
        for line in [
            "configuration: GENERIC (read offset: +6, C2 shift: 0)", "disc write offset: -12",
            "checking tracks", "errors detected, track: 01, sectors: {SKIP: 0, C2: 1}",
            "warning: CD-R trailing C2 errors detected (sectors: 1)", "done",
        ] { split.line(line); }
        let audio = AudioReport { rip: Some(("Test drive".into(), split.clone())), ctdb: None };
        let path = write_dump_report(&image, &temp.0, "Disc", audio, || Ok(())).unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("Drive read offset (stereo samples): +6"));
        assert!(text.contains("Disc write offset (stereo samples): -12"));
        assert!(text.contains("Track 01 | Expected CD-R boundary errors | Skipped sectors 0 | C2 sectors 1"));
        assert!(text.contains("Track 02 | No unresolved errors reported | Skipped sectors 0 | C2 sectors 0"));
        assert!(text.contains(&format!("PCM CRC32 {:08x}", crc32fast::hash(&[0; 2352]))));
        split.line("checking tracks");
        split.line("done");
        assert_eq!(split.track_audio_result(1), ("No unresolved errors reported", Some((0, 0))));
        split.line("checking tracks");
        split.line("errors detected, track: 02, sectors: {SKIP: 1, C2: 0}");
        split.line("done");
        assert_eq!(split.track_audio_result(2), ("Unresolved read errors", Some((1, 0))));
        split.line("checking tracks");
        assert_eq!(split.track_audio_result(2), ("Read evidence unavailable", None));
    }

    #[test]
    fn report_is_readable_preserves_previous_copy_and_respects_cancellation() {
        let dir = std::env::temp_dir().join(format!("dx-report-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&dir).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(dir.clone());
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso");
        let first = write_dump_report(&source, &dir, "Disc Name", AudioReport::default(), || Ok(())).unwrap();
        let text = fs::read_to_string(&first).unwrap();
        for label in ["Absolute latest date:", "Latest file date:", "Latest directory date:", "PVD creation date:", "Disc Info", "Tracks", "Report generated (UTC):"] { assert!(text.contains(label), "{label}"); }
        let second = write_dump_report(&source, &dir, "Disc Name", AudioReport::default(), || Ok(())).unwrap();
        assert_eq!(second.file_name().unwrap(), "Disc Name_disc-xplorer.2.txt");
        assert_eq!(fs::read_to_string(first).unwrap(), text);
        assert!(write_dump_report(&source, &dir, "Cancelled", AudioReport::default(), || Err("Stopped".into())).is_err());
        assert!(!dir.join("Cancelled_disc-xplorer.txt").exists());
        assert!(is_report_name("Disc Name_disc-xplorer.txt", "Disc Name"));
        assert!(is_report_name("Disc Name_disc-xplorer.2.txt", "Disc Name"));
        assert!(!is_report_name("Disc Name_disc-xplorer.notes.txt", "Disc Name"));
        assert!(!is_report_name("Other_disc-xplorer.txt", "Disc Name"));
    }
}
