//! Log archives with metadata-checked cleanup; recovery files stay on warnings.
use serde::Deserialize;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveMode {
    Off,
    Zip,
    #[default]
    #[serde(rename = "7z")]
    SevenZip,
}

// redumper's optional lead-in/out tracks use 0/00 and A/AA, with a session
// suffix on multisession discs. Do not confuse them with playable track 01.
// See redumper b761 cd/cd_split.ixx::write_tracks and cd/toc.ixx::printCUE.
fn boundary_track(filename: &str, name: &str) -> bool {
    let Some(track) = filename
        .strip_prefix(name)
        .and_then(|s| s.strip_prefix(" (Track "))
        .and_then(|s| s.strip_suffix(").bin"))
    else {
        return false;
    };
    let (track, session) = track
        .split_once('.')
        .map_or((track, None), |(t, s)| (t, Some(s)));
    matches!(track, "0" | "00" | "A" | "AA")
        && session.is_none_or(|s| {
            !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit())
                && s.parse::<u32>().is_ok_and(|n| n > 0)
        })
}

fn candidates(output: &Path, name: &str, verification: bool) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut folders = vec![PathBuf::new()];
    let verify = output.join("verification");
    if verification && fs::symlink_metadata(&verify).is_ok_and(|m| m.file_type().is_dir()) {
        folders.push(PathBuf::from("verification"));
    }
    for folder in folders {
        for entry in fs::read_dir(output.join(&folder)).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                continue;
            }
            let filename = entry.file_name();
            let Some(filename) = filename.to_str() else {
                continue;
            };
            let related = filename.starts_with(&format!("{name}."))
                || filename.starts_with(&format!("{name} ("))
                || crate::disc_report::is_report_name(filename, name)
                || (verification && filename == "verification.sha256");
            if !related {
                continue;
            }
            let ext = Path::new(filename)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "bin" {
                if boundary_track(filename, name) {
                    files.push(folder.join(filename));
                }
                continue;
            }
            if matches!(
                ext.as_str(),
                "7z" | "zip"
                    | "cue"
                    | "iso"
                    | "img"
                    | "mdf"
                    | "mds"
                    | "ccd"
                    | "gdi"
                    | "cdi"
                    | "nrg"
                    | "raw"
                    | "scram"
                    | "scrap"
                    | "sdram"
                    | "sbram"
            ) {
                continue;
            }
            files.push(folder.join(filename));
        }
    }
    files.sort();
    Ok(files)
}

pub fn find_7z() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(var) {
            dirs.push(PathBuf::from(dir).join("7-Zip"));
        }
    }
    let names = if cfg!(windows) {
        ["7zz.exe", "7z.exe", "7za.exe"]
    } else {
        ["7zz", "7z", "7za"]
    };
    dirs.into_iter()
        .flat_map(|dir| names.map(|name| dir.join(name)))
        .find(|p| p.is_file())
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(output: &Path) -> Result<Self, String> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = output.join(format!(".dx-archive-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_checked(
    mut input: impl Read,
    mut output: impl Write,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        check()?;
        let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
    }
}

fn write_zip(
    output: &Path,
    files: &[PathBuf],
    archive: &Path,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(archive)
        .map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    for path in files {
        check()?;
        let source = output.join(path);
        let meta = fs::symlink_metadata(&source).map_err(|e| e.to_string())?;
        if !meta.file_type().is_file() {
            return Err("An archive source is no longer a regular file.".into());
        }
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(meta.len() >= u32::MAX as u64);
        zip.start_file(path.to_string_lossy().replace('\\', "/"), options)
            .map_err(|e| e.to_string())?;
        copy_checked(
            File::open(source).map_err(|e| e.to_string())?,
            &mut zip,
            check,
        )?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

fn write_7z(
    tool: &Path,
    output: &Path,
    files: &[PathBuf],
    archive: &Path,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    let mut command = Command::new(tool);
    command
        .current_dir(output)
        .args(["a", "-t7z", "-mx=9", "-y", "-bd", "-spd"])
        .arg(archive)
        .arg("--")
        .args(files.iter().map(|p| Path::new(".").join(p)));
    run_7z(command, check)?;
    if !fs::metadata(archive).is_ok_and(|m| m.is_file() && m.len() > 0) {
        return Err("7z did not create an archive.".into());
    }
    Ok(())
}

fn run_7z(
    mut command: Command,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // No console window for the archiver.
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    loop {
        if let Err(error) = check() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("7z exited with {status}"))
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
}

fn publish(
    temp: &Path,
    output: &Path,
    name: &str,
    extension: &str,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<PathBuf, String> {
    let base = if name.to_ascii_lowercase().ends_with("_logs") {
        name.to_string()
    } else {
        format!("{name}_logs")
    };
    for index in 0..10000 {
        check()?;
        let suffix = if index == 0 {
            String::new()
        } else {
            format!("_{index}")
        };
        let dest = output.join(format!("{base}{suffix}.{extension}"));
        // The staging directory is on the same filesystem. Publishing a hard
        // link avoids copying a potentially large archive; it never replaces an
        // existing destination. Fall back for filesystems without hard links.
        match fs::hard_link(temp, &dest) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {}
        }
        let file = match OpenOptions::new().write(true).create_new(true).open(&dest) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        };
        let result = File::open(temp)
            .map_err(|e| e.to_string())
            .and_then(|input| copy_checked(input, file, check));
        if let Err(error) = result {
            let _ = fs::remove_file(&dest);
            return Err(error);
        }
        return Ok(dest);
    }
    Err("Too many log archives already exist in this folder.".into())
}

#[derive(Debug, PartialEq)]
struct SourceStamp {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

fn source_stamp(path: &Path) -> Result<SourceStamp, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("An archive source is no longer a regular file.".into());
    }
    Ok(SourceStamp {
        len: meta.len(),
        modified: meta.modified().map_err(|e| e.to_string())?,
        #[cfg(unix)]
        identity: {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
        },
    })
}

fn cleanup_archived(
    output: &Path,
    sources: &[(PathBuf, SourceStamp)],
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<String, String> {
    // Called only after the compressor finishes successfully and publication
    // succeeds. No extraction or content hashing: inspect just the original
    // candidate paths, keeping anything modified/replaced during compression.
    check()?;
    let mut kept = 0;
    for (path, expected) in sources {
        if check().is_err() {
            return Ok("Cleanup stopped. Remaining originals kept.".into());
        }
        let source = output.join(path);
        if source_stamp(&source).is_ok_and(|stamp| stamp == *expected)
            && fs::remove_file(&source).is_ok()
        {
            continue;
        }
        kept += 1;
    }
    Ok(if kept == 0 {
        "Archived originals removed.".into()
    } else {
        format!("{kept} original file(s) kept because they changed or could not be removed.")
    })
}

pub fn archive_logs(
    output: &Path,
    name: &str,
    verification: bool,
    mode: ArchiveMode,
    tool: Option<&Path>,
    remove_originals: bool,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<String, String> {
    if mode == ArchiveMode::Off {
        return Ok(String::new());
    }
    check()?;
    let files = candidates(output, name, verification)?;
    if files.is_empty() {
        return Ok("No log files were available to archive.".into());
    }
    let sources = if remove_originals {
        files
            .iter()
            .map(|path| {
                check()?;
                source_stamp(&output.join(path)).map(|stamp| (path.clone(), stamp))
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let temp = TempDir::new(output)?;
    let mut fallback = false;
    if mode == ArchiveMode::SevenZip {
        let archive = temp.0.join("logs.7z");
        if let Some(tool) = tool {
            if write_7z(tool, output, &files, &archive, &mut check).is_ok() {
                let path = publish(&archive, output, name, "7z", &mut check)?;
                let cleanup = if remove_originals {
                    cleanup_archived(output, &sources, &mut check)?
                } else {
                    "Originals kept for refinement.".into()
                };
                return Ok(format!(
                    "Logs archived as {}. {cleanup}",
                    path.file_name().unwrap().to_string_lossy(),
                ));
            }
        }
        check()?; // Cancellation must never start the fallback compressor.
        fallback = true;
    }
    let archive = temp.0.join("logs.zip");
    write_zip(output, &files, &archive, &mut check)?;
    let path = publish(&archive, output, name, "zip", &mut check)?;
    let cleanup = if remove_originals {
        cleanup_archived(output, &sources, &mut check)?
    } else {
        "Originals kept for refinement.".into()
    };
    Ok(format!(
        "Logs archived as {}{}. {cleanup}",
        path.file_name().unwrap().to_string_lossy(),
        if fallback {
            " (ZIP fallback: 7z unavailable or failed)"
        } else {
            ""
        }
    ))
}

// Only summarize our generated archive result, never arbitrary process output.
// Match the cleanup suffix so a filename cannot affect the reported outcome.
pub fn status_summary(message: &str) -> &str {
    if message.starts_with("Logs archived as ") {
        if message.ends_with("Archived originals removed.") {
            "Logs archived."
        } else {
            "Logs archived; some originals kept."
        }
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "dx-archive-test-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, bytes: &[u8]) {
            fs::write(self.0.join(name), bytes).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn selection_excludes_images_archives_unrelated_files_and_links() {
        let f = Fixture::new();
        for name in [
            "Disc.log",
            "Disc.state",
            "Disc.subcode",
            "Disc.toc",
            "Disc.skeleton",
            "Disc.iso",
            "Disc.scram",
            "Disc.sdram",
            "Disc.sbram",
            "Disc (Track 01).bin",
            "Disc.cue",
            "Disc.zip",
            "Disc.7z",
            "Other.log",
            "Disco.log",
        ] {
            f.write(name, b"data");
        }
        fs::create_dir(f.0.join("Disc.directory")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("Other.log", f.0.join("Disc.link")).unwrap();
        let files = candidates(&f.0, "Disc", false).unwrap();
        assert_eq!(
            files,
            [
                "Disc.log",
                "Disc.skeleton",
                "Disc.state",
                "Disc.subcode",
                "Disc.toc"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn boundary_tracks_are_archived_and_deleted_without_removing_normal_tracks() {
        let f = Fixture::new();
        f.write("Disc.cue", b"FILE \"Disc (Track 01).bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 02:00:00\n");
        for path in [
            "Disc (Track 0).bin",
            "Disc (Track 00).bin",
            "Disc (Track A).bin",
            "Disc (Track AA).bin",
            "Disc (Track 00.1).bin",
            "Disc (Track AA.2).bin",
            "Disc (Track 01).bin",
            "Disc (Track 10).bin",
            "Disc (Track 100).bin",
            "Disc (Track 0.backup).bin",
            "Disc (Track 0.0).bin",
            "Other (Track 0).bin",
        ] {
            f.write(path, b"track contents");
        }
        // Archive boundary extras even if an alternate CUE references them.
        f.write(
            "Disc_09.cue",
            b"FILE \".\\DISC (TRACK 00).BIN\" BINARY\nTRACK 00 AUDIO\nINDEX 01 00:00:00\n",
        );
        let expected = [
            "Disc (Track 0).bin",
            "Disc (Track 00).bin",
            "Disc (Track 00.1).bin",
            "Disc (Track A).bin",
            "Disc (Track AA).bin",
            "Disc (Track AA.2).bin",
        ]
        .map(PathBuf::from);
        assert_eq!(candidates(&f.0, "Disc", false).unwrap(), expected);
        let message =
            archive_logs(&f.0, "Disc", false, ArchiveMode::Zip, None, true, || Ok(())).unwrap();
        assert!(message.contains("Archived originals removed"));
        let mut zip = zip::ZipArchive::new(File::open(f.0.join("Disc_logs.zip")).unwrap()).unwrap();
        for path in expected {
            assert!(!f.0.join(&path).exists());
            let mut bytes = Vec::new();
            zip.by_name(path.to_str().unwrap())
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"track contents");
        }
        for path in [
            "Disc (Track 01).bin",
            "Disc (Track 10).bin",
            "Disc (Track 100).bin",
            "Other (Track 0).bin",
            "Disc.cue",
            "Disc_09.cue",
        ] {
            assert!(f.0.join(path).is_file(), "{path}");
        }
    }

    #[test]
    fn boundary_selection_needs_no_cue_and_does_not_recurse() {
        let f = Fixture::new();
        f.write("Disc (Track 0).bin", b"hidden data");
        assert_eq!(
            candidates(&f.0, "Disc", false).unwrap(),
            [PathBuf::from("Disc (Track 0).bin")]
        );
        f.write("Disc.cue", b"malformed cue");
        assert_eq!(
            candidates(&f.0, "Disc", false).unwrap(),
            [PathBuf::from("Disc (Track 0).bin")]
        );
        f.write("Disc.cue", b"FILE \"Disc.bin\" BINARY\n");
        fs::create_dir(f.0.join("other")).unwrap();
        f.write("other/Disc.log", b"unrelated nested file");
        fs::create_dir(f.0.join("verification")).unwrap();
        f.write(
            "verification/Disc.cue",
            b"FILE \"Disc (Track 0).bin\" BINARY\n",
        );
        f.write("verification/Disc (Track 0).bin", b"hidden data");
        f.write("verification/Disc (Track A).bin", b"leadout");
        assert_eq!(
            candidates(&f.0, "Disc", false).unwrap(),
            [PathBuf::from("Disc (Track 0).bin")]
        );
        assert_eq!(
            candidates(&f.0, "Disc", true).unwrap(),
            [
                PathBuf::from("Disc (Track 0).bin"),
                PathBuf::from("verification/Disc (Track 0).bin"),
                PathBuf::from("verification/Disc (Track A).bin")
            ]
        );
    }

    #[test]
    fn zip_fallback_keeps_originals_and_includes_both_passes() {
        let f = Fixture::new();
        f.write("Disc.log", b"first log");
        f.write("Disc.state", b"recovery");
        f.write("Disc.iso", b"image");
        fs::create_dir(f.0.join("verification")).unwrap();
        f.write("verification/Disc.log", b"second log");
        f.write("verification/Disc.iso", b"image");
        f.write("verification.sha256", b"hashes");
        let message = archive_logs(
            &f.0,
            "Disc",
            true,
            ArchiveMode::SevenZip,
            None,
            false,
            || Ok(()),
        )
        .unwrap();
        assert!(message.contains("ZIP fallback"));
        assert_eq!(
            status_summary(&message),
            "Logs archived; some originals kept."
        );
        let mut archive =
            zip::ZipArchive::new(File::open(f.0.join("Disc_logs.zip")).unwrap()).unwrap();
        assert_eq!(archive.len(), 4);
        for (name, expected) in [
            ("Disc.log", "first log"),
            ("Disc.state", "recovery"),
            ("verification/Disc.log", "second log"),
            ("verification.sha256", "hashes"),
        ] {
            let mut value = String::new();
            archive
                .by_name(name)
                .unwrap()
                .read_to_string(&mut value)
                .unwrap();
            assert_eq!(value, expected);
            assert_eq!(fs::read(f.0.join(name)).unwrap(), expected.as_bytes());
        }
        assert!(archive.by_name("Disc.iso").is_err());
        assert_eq!(fs::read(f.0.join("Disc.iso")).unwrap(), b"image");
    }

    #[test]
    fn failed_7z_falls_back_and_archives_never_overwrite() {
        let f = Fixture::new();
        f.write("Disc_logs.log", b"log");
        f.write("Disc_logs.zip", b"existing archive");
        let unavailable = f.0.join("missing-7z");
        let message = archive_logs(
            &f.0,
            "Disc_logs",
            false,
            ArchiveMode::SevenZip,
            Some(&unavailable),
            false,
            || Ok(()),
        )
        .unwrap();
        assert!(message.contains("Disc_logs_1.zip"));
        assert!(message.contains("ZIP fallback"));
        assert_eq!(
            status_summary(&message),
            "Logs archived; some originals kept."
        );
        assert_eq!(
            fs::read(f.0.join("Disc_logs.zip")).unwrap(),
            b"existing archive"
        );
        let archive =
            zip::ZipArchive::new(File::open(f.0.join("Disc_logs_1.zip")).unwrap()).unwrap();
        assert_eq!(archive.len(), 1);
        assert!(fs::read_dir(&f.0).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".dx-archive-")));
    }

    #[test]
    fn zip_cleanup_removes_only_archived_files_in_both_passes() {
        let f = Fixture::new();
        f.write("Disc_disc-xplorer.txt", b"Disc Xplorer Report");
        f.write("Disc_disc-xplorer.2.txt", b"Refined report");
        f.write("Other_disc-xplorer.txt", b"unrelated report");
        f.write("Disc.log", b"log");
        f.write("Disc.state", b"state");
        f.write("Disc.iso", b"image");
        f.write("Disc.scram", b"raw image");
        f.write("Other.log", b"unrelated");
        f.write("Disc_logs.zip", b"old archive");
        fs::create_dir(f.0.join("verification")).unwrap();
        f.write("verification/Disc.log", b"second log");
        f.write("verification/Disc.iso", b"second image");
        let message = archive_logs(
            &f.0,
            "Disc",
            true,
            ArchiveMode::SevenZip,
            None,
            true,
            || Ok(()),
        )
        .unwrap();
        assert!(message.contains("ZIP fallback"));
        assert!(message.contains("Archived originals removed"));
        assert_eq!(status_summary(&message), "Logs archived.");
        for path in ["Disc.log", "Disc.state", "verification/Disc.log", "Disc_disc-xplorer.txt", "Disc_disc-xplorer.2.txt"] {
            assert!(!f.0.join(path).exists(), "{path}");
        }
        for path in [
            "Disc.iso",
            "Disc.scram",
            "Other.log",
            "Other_disc-xplorer.txt",
            "Disc_logs.zip",
            "verification/Disc.iso",
        ] {
            assert!(f.0.join(path).is_file(), "{path}");
        }
        let mut zip =
            zip::ZipArchive::new(File::open(f.0.join("Disc_logs_1.zip")).unwrap()).unwrap();
        let mut text = String::new();
        zip.by_name("Disc.log")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "log");
        let mut report = String::new();
        zip.by_name("Disc_disc-xplorer.txt").unwrap().read_to_string(&mut report).unwrap();
        assert_eq!(report, "Disc Xplorer Report");
    }

    #[test]
    fn cleanup_keeps_sources_on_cancellation_or_changed_metadata() {
        let f = Fixture::new();
        f.write("Disc.log", b"original");
        let sources = vec![(
            PathBuf::from("Disc.log"),
            source_stamp(&f.0.join("Disc.log")).unwrap(),
        )];
        assert!(cleanup_archived(&f.0, &sources, &mut || Err("stopped".into())).is_err());
        assert!(f.0.join("Disc.log").exists());
        f.write("Disc.log", b"changed after compression");
        let message = cleanup_archived(&f.0, &sources, &mut || Ok(())).unwrap();
        assert!(message.contains("1 original file(s) kept"));
        assert_eq!(
            fs::read(f.0.join("Disc.log")).unwrap(),
            b"changed after compression"
        );
        #[cfg(unix)]
        {
            fs::remove_file(f.0.join("Disc.log")).unwrap();
            f.write("Other.log", b"original");
            std::os::unix::fs::symlink("Other.log", f.0.join("Disc.log")).unwrap();
            let message = cleanup_archived(&f.0, &sources, &mut || Ok(())).unwrap();
            assert!(message.contains("1 original file(s) kept"));
            assert!(f.0.join("Disc.log").is_symlink());
            assert_eq!(fs::read(f.0.join("Other.log")).unwrap(), b"original");
        }
    }

    #[test]
    fn off_and_cancelled_archives_leave_no_outputs_and_keep_sources() {
        let f = Fixture::new();
        f.write("Disc.log", &vec![1; 3 * 1024 * 1024]);
        assert_eq!(
            archive_logs(
                &f.0,
                "Disc",
                false,
                ArchiveMode::Off,
                None,
                false,
                || panic!("off must do nothing")
            )
            .unwrap(),
            ""
        );
        let mut calls = 0;
        assert!(
            archive_logs(&f.0, "Disc", false, ArchiveMode::Zip, None, false, || {
                calls += 1;
                if calls >= 5 {
                    Err("stopped".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
        assert_eq!(
            fs::metadata(f.0.join("Disc.log")).unwrap().len(),
            3 * 1024 * 1024
        );
    }

    #[test]
    fn installed_7z_archives_literal_filenames() {
        let Some(tool) = find_7z() else {
            eprintln!("7z unavailable; real 7z integration skipped");
            return;
        };
        let f = Fixture::new();
        f.write("@Disc [1].log", b"log contents");
        let message = archive_logs(
            &f.0,
            "@Disc [1]",
            false,
            ArchiveMode::SevenZip,
            Some(&tool),
            false,
            || Ok(()),
        )
        .unwrap();
        assert!(message.contains("@Disc [1]_logs.7z"), "{message}");
        assert!(!message.contains("fallback"));
        let marked = archive_logs(
            &f.0,
            "@Disc [1]",
            false,
            ArchiveMode::SevenZip,
            Some(&tool),
            true,
            || Ok(()),
        )
        .unwrap();
        assert!(marked.contains("@Disc [1]_logs_1.7z"));
        assert!(marked.contains("Archived originals removed"));
        assert!(!marked.contains("fallback"));
        let archive = f.0.join("@Disc [1]_logs_1.7z");
        let result = Command::new(tool)
            .args(["x", "-so"])
            .arg(archive)
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"log contents");
        assert!(!f.0.join("@Disc [1].log").exists());
        f.write("@Disc [1].log", b"log contents");
        let tool = find_7z().unwrap();
        let mut checks = 0;
        assert!(archive_logs(
            &f.0,
            "@Disc [1]",
            false,
            ArchiveMode::SevenZip,
            Some(&tool),
            false,
            || {
                checks += 1;
                if checks >= 3 {
                    Err("stopped".into())
                } else {
                    Ok(())
                }
            }
        )
        .is_err());
        assert_eq!(
            fs::read_dir(&f.0).unwrap().count(),
            3,
            "cancel removes temporary files and never falls back to ZIP"
        );
    }
}
