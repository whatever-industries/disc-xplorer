//! Run redumper's read-only skeleton command with a private CUE in a new output
//! folder. Absolute FILE references avoid copying image data or touching its log.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};

#[derive(Default)]
pub struct SkeletonState {
    running: AtomicBool,
    cancelled: AtomicBool,
    child: Mutex<Option<CommandChild>>,
}

struct RunGuard<'a>(&'a SkeletonState);
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.child.lock().unwrap().take();
        self.0.running.store(false, Ordering::SeqCst);
    }
}

fn input_cue(path: &Path) -> Result<String, String> {
    let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !path.is_file() { return Err("Open an ISO or CUE image first.".into()); }
    if path.to_string_lossy().contains(['"', '\r', '\n']) { return Err("This image path cannot be represented in a CUE sheet.".into()); }
    let text = match ext_of(&path).as_str() {
        "iso" => format!("FILE \"{}\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n", path.to_string_lossy()),
        "cue" => fs::read_to_string(&path).map_err(|e| e.to_string())?,
        _ => return Err("Skeleton creation supports ISO and CUE/BIN images.".into()),
    };
    let mut result = String::new();
    let mut file = None;
    let mut seen = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        let upper = line.to_uppercase();
        if upper.starts_with("FILE ") {
            let name = extract_quoted(line).ok_or("Invalid CUE file reference.")?;
            file = Some(fs::canonicalize(resolve_cue_file(path.parent().unwrap(), name)).map_err(|e| e.to_string())?);
        } else if upper.starts_with("TRACK ") {
            let mode = upper.split_whitespace().nth(2).unwrap_or("");
            if mode == "AUDIO" { continue; }
            if !matches!(mode, "MODE1/2048" | "MODE1/2352" | "MODE2/2352" | "CDI/2352") {
                return Err("This track format is not supported for skeleton creation.".into());
            }
            let input = file.as_ref().ok_or("Invalid CUE file reference.")?;
            // redumper operates once per FILE, not once per INDEX/track. Keep
            // shared-BIN images to one entry, matching its native CUE behavior.
            if !seen.insert(input.clone()) { continue; }
            let name = input.file_stem().ok_or("Invalid image filename.")?.to_string_lossy().to_string();
            if !names.insert(name.to_lowercase()) { return Err("Track filenames must be unique for skeleton creation.".into()); }
            let input_text = input.to_string_lossy();
            if input_text.contains(['"', '\r', '\n']) { return Err("This image path cannot be represented in a CUE sheet.".into()); }
            let stride = if mode == "MODE1/2048" { 2048 } else { 2352 };
            let len = fs::metadata(input).map_err(|e| e.to_string())?.len();
            if len == 0 || len % stride != 0 { return Err("Image size does not match its CUE sector format.".into()); }
            result.push_str(&format!("FILE \"{input_text}\" BINARY\n  TRACK 01 {mode}\n    INDEX 01 00:00:00\n"));
        }
    }
    if result.is_empty() { return Err("No supported data tracks found.".into()); }
    Ok(result)
}

#[tauri::command]
pub fn cancel_skeleton(state: tauri::State<'_, SkeletonState>) {
    state.cancelled.store(true, Ordering::SeqCst);
    if let Some(child) = state.child.lock().unwrap().take() { let _ = child.kill(); }
}

#[tauri::command]
pub async fn create_skeleton(
    image_path: String, output_path: String, source: String, external_path: Option<String>,
    progress: tauri::ipc::Channel<String>, app: tauri::AppHandle,
    state: tauri::State<'_, SkeletonState>,
) -> Result<Vec<String>, String> {
    if state.running.swap(true, Ordering::SeqCst) { return Err("Skeleton creation is already running.".into()); }
    let _guard = RunGuard(&state);
    state.cancelled.store(false, Ordering::SeqCst);
    let cue = input_cue(Path::new(&image_path))?;
    let output = PathBuf::from(output_path);
    // Never reuse a folder: redumper also writes a log before checking outputs.
    fs::create_dir(&output).map_err(|e| if e.kind() == io::ErrorKind::AlreadyExists {
        "The skeleton output folder already exists. Choose another destination.".to_string()
    } else { format!("Cannot create output folder: {e}") })?;
    let output = fs::canonicalize(output).map_err(|e| e.to_string())?;
    let cue_path = output.join("skeleton-job.cue");
    fs::write(&cue_path, cue).map_err(|e| e.to_string())?;
    let result = async {
        let (mut rx, child) = redumper_cmd(&source, external_path.as_deref(), &app)?
            .args(["skeleton".to_string(), format!("--image-path={}", output.display()), "--image-name=skeleton-job".to_string()])
            .spawn().map_err(|e| e.to_string())?;
        {
            let mut slot = state.child.lock().unwrap();
            if state.cancelled.load(Ordering::SeqCst) { let _ = child.kill(); }
            else { *slot = Some(child); }
        }
        let mut last_update = std::time::Instant::now();
        let mut success = false;
        let mut last_error = String::new();
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some(line) = text.split(['\r', '\n']).rfind(|s| !s.trim().is_empty()) {
                        // Keep transient progress output bounded.
                        let line: String = line.chars().take(400).collect();
                        if line.to_lowercase().contains("error") { last_error = line.clone(); }
                        if !line.contains('%') || last_update.elapsed().as_millis() >= 100 {
                            let _ = progress.send(line);
                            last_update = std::time::Instant::now();
                        }
                    }
                }
                CommandEvent::Error(e) => last_error = e,
                CommandEvent::Terminated(status) => { success = status.code == Some(0); break; }
                _ => {}
            }
        }
        if !success || state.cancelled.load(Ordering::SeqCst) {
            return Err(if last_error.is_empty() { "Skeleton creation stopped. Partial files are in the output folder.".into() } else { last_error });
        }
        let mut files = Vec::new();
        for entry in fs::read_dir(&output).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if matches!(ext_of(&path).as_str(), "skeleton" | "hash") { files.push(path.to_string_lossy().into_owned()); }
        }
        if !files.iter().any(|p| p.ends_with(".skeleton")) {
            return Err("No skeleton created. Redumper requires an ISO 9660 filesystem.".into());
        }
        files.sort();
        Ok(files)
    }.await;
    let _ = fs::remove_file(cue_path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires DX_REDUMPER pointing to a runnable redumper binary"]
    fn real_redumper_preserves_source_and_filesystem_metadata() {
        let exe = std::env::var("DX_REDUMPER").unwrap();
        let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso");
        let original = fs::read(&input).unwrap();
        let output = std::env::temp_dir().join(format!("dx-skeleton-integration-{}", std::process::id()));
        fs::create_dir(&output).unwrap();
        fs::write(output.join("job.cue"), input_cue(&input).unwrap()).unwrap();
        let status = std::process::Command::new(exe)
            .args(["skeleton".into(), format!("--image-path={}", output.display()), "--image-name=job".into()])
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
        assert!(status.success());
        assert_eq!(fs::read(&input).unwrap(), original);
        let skeleton = fs::read(output.join("test.skeleton")).unwrap();
        assert_eq!(skeleton.len(), original.len());
        assert_eq!(&skeleton[16 * 2048..17 * 2048], &original[16 * 2048..17 * 2048]);
        let root = u32::from_le_bytes(original[16 * 2048 + 158..16 * 2048 + 162].try_into().unwrap()) as usize;
        assert_eq!(&skeleton[root * 2048..(root + 1) * 2048], &original[root * 2048..(root + 1) * 2048]);
        assert!(skeleton.iter().zip(&original).any(|(new, old)| *new == 0 && *old != 0));
        assert!(fs::metadata(output.join("test.hash")).unwrap().len() > 0);
        // A second invocation must refuse to overwrite the generated artifacts.
        let status = std::process::Command::new(std::env::var("DX_REDUMPER").unwrap())
            .args(["skeleton".into(), format!("--image-path={}", output.display()), "--image-name=job".into()])
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
        assert!(!status.success());
        assert_eq!(fs::read(output.join("test.skeleton")).unwrap(), skeleton);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn iso_uses_absolute_read_only_reference() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso");
        let cue = input_cue(&path).unwrap();
        assert!(cue.contains("MODE1/2048"));
        assert!(cue.contains(&fs::canonicalize(path).unwrap().to_string_lossy().to_string()));
    }
    #[test]
    fn cue_deduplicates_shared_bin_and_skips_audio() {
        let dir = std::env::temp_dir().join(format!("dx-skeleton-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("disc.bin"), vec![0; 2352 * 20]).unwrap();
        fs::write(dir.join("disc.cue"), "FILE \"disc.bin\" BINARY\n TRACK 01 AUDIO\n TRACK 02 MODE1/2352\n TRACK 03 MODE1/2352\n").unwrap();
        let cue = input_cue(&dir.join("disc.cue")).unwrap();
        assert_eq!(cue.matches("FILE ").count(), 1);
        assert!(!cue.contains("AUDIO"));
        fs::write(dir.join("disc.bin"), vec![0; 2448]).unwrap();
        assert!(input_cue(&dir.join("disc.cue")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
