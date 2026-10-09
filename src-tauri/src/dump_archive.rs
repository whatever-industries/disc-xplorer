//! Log archives with verified cleanup for good dumps; recovery files stay on warnings.
use serde::Deserialize;
use sha2::{Digest, Sha256};
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
                || (verification && filename == "verification.sha256");
            if !related {
                continue;
            }
            let ext = Path::new(filename)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                ext.as_str(),
                "7z" | "zip"
                    | "bin"
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
    if !archive.is_file() {
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

fn fingerprint(
    mut source: impl Read,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        check()?;
        let n = source.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(hash.finalize().to_vec());
        }
        hash.update(&buffer[..n]);
    }
}

fn source_fingerprint(
    path: &Path,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("An archive source is no longer a regular file.".into());
    }
    fingerprint(File::open(path).map_err(|e| e.to_string())?, check)
}

fn cleanup_verified(
    output: &Path,
    archive: &Path,
    sources: &[(PathBuf, Vec<u8>)],
    tool: Option<&Path>,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<String, String> {
    // Read back the published archive and match every entry against its original
    // content before deleting anything. A compressor's success code is not enough.
    if archive.extension().is_some_and(|e| e == "7z") {
        let temp = TempDir::new(output)?;
        let mut command = Command::new(tool.ok_or("7z unavailable for verification")?);
        command
            .args(["x", "-y", "-bd"])
            .arg(format!("-o{}", temp.0.display()))
            .arg("--")
            .arg(archive);
        run_7z(command, check)?;
        for (path, expected) in sources {
            if source_fingerprint(&temp.0.join(path), check)? != *expected {
                return Err("Archive verification failed; originals kept.".into());
            }
        }
    } else {
        let mut zip = zip::ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        for (path, expected) in sources {
            let entry = zip
                .by_name(&path.to_string_lossy().replace('\\', "/"))
                .map_err(|e| e.to_string())?;
            if fingerprint(entry, check)? != *expected {
                return Err("Archive verification failed; originals kept.".into());
            }
        }
    }
    check()?;
    let mut kept = 0;
    for (path, expected) in sources {
        // Keep any file edited/replaced since compression began. Never remove
        // image files or unrelated files: only the exact archived candidate set.
        if check().is_err() {
            return Ok("Archive verified; cleanup stopped. Remaining originals kept.".into());
        }
        let source = output.join(path);
        if source_fingerprint(&source, check).is_ok_and(|hash| hash == *expected)
            && fs::remove_file(&source).is_ok()
        {
            continue;
        }
        kept += 1;
    }
    Ok(if kept == 0 {
        "Archive verified; archived originals removed.".into()
    } else {
        format!("Archive verified; {kept} original file(s) kept because they changed or could not be removed.")
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
                source_fingerprint(&output.join(path), &mut check).map(|hash| (path.clone(), hash))
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
                    cleanup_verified(output, &path, &sources, Some(tool), &mut check)?
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
        cleanup_verified(output, &path, &sources, None, &mut check)?
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
        if message.ends_with("Archive verified; archived originals removed.") {
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
        assert_eq!(status_summary(&message), "Logs archived; some originals kept.");
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
        assert_eq!(status_summary(&message), "Logs archived; some originals kept.");
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
    fn verified_zip_cleanup_removes_only_archived_files_in_both_passes() {
        let f = Fixture::new();
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
        assert!(message.contains("archived originals removed"));
        assert_eq!(status_summary(&message), "Logs archived.");
        for path in ["Disc.log", "Disc.state", "verification/Disc.log"] {
            assert!(!f.0.join(path).exists(), "{path}");
        }
        for path in [
            "Disc.iso",
            "Disc.scram",
            "Other.log",
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
    }

    #[test]
    fn cleanup_keeps_sources_on_corruption_cancellation_or_changed_content() {
        let f = Fixture::new();
        f.write("Disc.log", b"original");
        let sources = vec![(
            PathBuf::from("Disc.log"),
            source_fingerprint(&f.0.join("Disc.log"), &mut || Ok(())).unwrap(),
        )];
        let archive = f.0.join("Disc_logs.zip");
        f.write("Disc_logs.zip", b"broken archive");
        assert!(cleanup_verified(&f.0, &archive, &sources, None, &mut || Ok(())).is_err());
        assert_eq!(fs::read(f.0.join("Disc.log")).unwrap(), b"original");
        fs::remove_file(&archive).unwrap();
        write_zip(&f.0, &[PathBuf::from("Disc.log")], &archive, &mut || Ok(())).unwrap();
        assert!(cleanup_verified(
            &f.0,
            &archive,
            &sources,
            None,
            &mut || Err("stopped".into())
        )
        .is_err());
        assert!(f.0.join("Disc.log").exists());
        f.write("Disc.log", b"changed after compression");
        let message = cleanup_verified(&f.0, &archive, &sources, None, &mut || Ok(())).unwrap();
        assert!(message.contains("1 original file(s) kept"));
        assert_eq!(
            fs::read(f.0.join("Disc.log")).unwrap(),
            b"changed after compression"
        );
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
        assert!(marked.contains("archived originals removed"));
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
