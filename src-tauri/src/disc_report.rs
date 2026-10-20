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

pub(crate) fn write_dump_report(image: &Path, output: &Path, name: &str, mut check: impl FnMut() -> Result<(), String>) -> Result<PathBuf, String> {
    check()?;
    let image_path = image.to_string_lossy().into_owned();
    let info = disc_info::collect_info(&image_path)?;
    check()?;
    let dates = disc_date_report(image_path, None)?;
    let text = render(image, &info, &dates);
    check()?;
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
    fn report_is_readable_preserves_previous_copy_and_respects_cancellation() {
        let dir = std::env::temp_dir().join(format!("dx-report-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&dir).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(dir.clone());
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso");
        let first = write_dump_report(&source, &dir, "Disc Name", || Ok(())).unwrap();
        let text = fs::read_to_string(&first).unwrap();
        for label in ["Absolute latest date:", "Latest file date:", "Latest directory date:", "PVD creation date:", "Disc Info", "Tracks", "Report generated (UTC):"] { assert!(text.contains(label), "{label}"); }
        let second = write_dump_report(&source, &dir, "Disc Name", || Ok(())).unwrap();
        assert_eq!(second.file_name().unwrap(), "Disc Name_disc-xplorer.2.txt");
        assert_eq!(fs::read_to_string(first).unwrap(), text);
        assert!(write_dump_report(&source, &dir, "Cancelled", || Err("Stopped".into())).is_err());
        assert!(!dir.join("Cancelled_disc-xplorer.txt").exists());
        assert!(is_report_name("Disc Name_disc-xplorer.txt", "Disc Name"));
        assert!(is_report_name("Disc Name_disc-xplorer.2.txt", "Disc Name"));
        assert!(!is_report_name("Disc Name_disc-xplorer.notes.txt", "Disc Name"));
        assert!(!is_report_name("Other_disc-xplorer.txt", "Disc Name"));
    }
}
