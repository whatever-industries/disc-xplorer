//! One dump job owned by the application, independent of the current UI mode.
#[path = "dump_archive.rs"]
mod archive;
#[path = "dump_assessment.rs"]
mod assessment;
#[path = "dump_command.rs"]
mod command;
use archive::ArchiveMode;
use assessment::SplitAssessment;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};

#[derive(Clone, Default, Debug, Serialize)]
pub struct DumpProgress {
    percentage: Option<f64>,
    current: Option<i64>,
    total: Option<i64>,
    scsi: Option<u64>,
    edc: Option<u64>,
    c2: Option<u64>,
    q: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct ReadErrorSummary {
    refining: bool,
    first_progress_seen: bool,
    initial: DumpProgress,
    corrections: Option<bool>,
}

impl ReadErrorSummary {
    fn line(&mut self, line: &str, live: Option<&DumpProgress>, progress: &mut DumpProgress) {
        if let Some(stage) = line.strip_prefix("*** ") {
            self.refining = stage.split_whitespace().next() == Some("REFINE");
            self.first_progress_seen = false;
            self.initial = progress.clone();
            self.corrections = None;
        }
        if let Some(live) = live {
            if self.refining && !self.first_progress_seen {
                // redumper prints this before the first read of the refine pass.
                self.initial = live.clone();
                self.first_progress_seen = true;
            }
        }
        if line == "media errors:" {
            self.corrections = Some(false);
            return;
        }
        if line == "correction statistics:" && self.refining {
            self.corrections = Some(true);
            return;
        }
        let Some(corrections) = self.corrections else {
            return;
        };
        for (label, current, initial) in [
            ("SCSI:", &mut progress.scsi, self.initial.scsi),
            ("EDC:", &mut progress.edc, self.initial.edc),
            ("C2:", &mut progress.c2, self.initial.c2),
            ("Q:", &mut progress.q, self.initial.q),
        ] {
            if line.starts_with(label) {
                if let Some(count) = number_after(line, label) {
                    // Correction totals are recovered errors, not remaining errors.
                    // The last live update precedes the last read and can be stale.
                    let remaining = if corrections {
                        initial.and_then(|n| n.checked_sub(count))
                    } else {
                        Some(count)
                    };
                    if let Some(remaining) = remaining {
                        *current = Some(remaining);
                    }
                }
                return;
            }
        }
        self.corrections = None;
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DumpJob {
    id: u64,
    revision: u64,
    pub drive: String,
    drive_name: String,
    name: String,
    output_path: String,
    image_path: Option<String>,
    can_refine: bool,
    output_exists: bool,
    status: String,
    stage: String,
    message: String,
    command: String,
    started_at: u64,
    finished_at: Option<u64>,
    progress: DumpProgress,
    logs: Vec<String>,
    warnings: bool,
    #[serde(skip)]
    dvd_or_bd: bool,
    #[serde(skip)]
    pass: u8,
    #[serde(skip)]
    split_assessment: SplitAssessment,
    #[serde(skip)]
    read_error_summary: ReadErrorSummary,
}

impl DumpJob {
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "running" | "stopping")
    }

    fn line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        self.split_assessment.line(line);
        if let Some(stage) = line.strip_prefix("*** ") {
            self.stage = stage.split('(').next().unwrap_or(stage).trim().to_string();
            if self.stage == "SPLIT" {
                self.stage = if self.dvd_or_bd { "Generating .iso file" } else { "Generating .bin files" }.into();
            }
            if self.pass > 0 {
                self.stage = format!("Dump {} · {}", self.pass, self.stage);
            }
            // Each pass has its own percentage. Reading 100% is not job completion.
            self.progress.percentage = None;
            self.progress.current = None;
            self.progress.total = None;
        }
        let progress = parse_progress(line);
        if let Some(p) = &progress {
            self.progress.percentage = p.percentage;
            self.progress.current = p.current;
            self.progress.total = p.total;
            if p.scsi.is_some() {
                self.progress.scsi = p.scsi;
            }
            if p.edc.is_some() {
                self.progress.edc = p.edc;
            }
            if p.c2.is_some() {
                self.progress.c2 = p.c2;
            }
            if p.q.is_some() {
                self.progress.q = p.q;
            }
        }
        self.read_error_summary
            .line(line, progress.as_ref(), &mut self.progress);
        if let Some(profile) = line.strip_prefix("profile:") {
            self.dvd_or_bd = double_dump_media(profile);
        }
        let lower = line.to_ascii_lowercase();
        self.warnings |= lower.starts_with("warning:")
            || lower.starts_with("error:")
            || self.split_assessment.blocked;
        if progress.is_some()
            && self
                .logs
                .last()
                .is_some_and(|last| parse_progress(last).is_some())
        {
            self.logs.pop();
        }
        // Keep the UI bounded; redumper also writes its full log to the dump folder.
        self.logs.push(line.chars().take(4096).collect());
        if self.logs.len() > 400 {
            self.logs.remove(0);
        }
        self.revision += 1;
    }

    fn prepare_log_archive(&mut self) -> Result<PathBuf, &'static str> {
        // Forced splitting can exit successfully with unresolved track errors.
        // Wait for a clean refinement; ordinary warnings are still archivable.
        if self.split_assessment.blocked || (self.dvd_or_bd && has_errors(&self.progress)) {
            return Err("Log compression skipped: unresolved track errors remain. Original files have been kept for refinement.");
        }
        self.pass = 0;
        self.line("*** Compressing logs");
        Ok(PathBuf::from(&self.output_path))
    }

    fn refresh_files(&mut self) {
        if self.active() {
            return;
        }
        let output = Path::new(&self.output_path);
        let exists = output.is_dir();
        let image = completed_image(output, &self.name).map(|p| p.to_string_lossy().into_owned());
        let refine = needs_review(&self.status) && refinable_files(output, &self.name);
        if self.output_exists != exists || self.image_path != image || self.can_refine != refine {
            self.output_exists = exists;
            self.image_path = image;
            self.can_refine = refine;
            self.revision += 1;
        }
    }

    fn finish(&mut self, code: Option<i32>) {
        self.finished_at = Some(now_ms());
        self.image_path = completed_image(Path::new(&self.output_path), &self.name)
            .map(|p| p.to_string_lossy().into_owned());
        let errors = [
            self.progress.scsi,
            self.progress.edc,
            self.progress.c2,
            self.progress.q,
        ]
        .into_iter()
        .flatten()
        .any(|n| n > 0);
        let (status, message) = if self.status == "stopping" {
            (
                "cancelled",
                "Dump stopped. Partial files kept.".to_string(),
            )
        } else if code != Some(0) {
            (
                "failed",
                format!(
                    "Dump failed ({}). Partial files kept; see log.",
                    code.map(|n| format!("code {n}"))
                        .unwrap_or_else(|| "no exit code".into())
                ),
            )
        } else if self.warnings || errors || self.image_path.is_none() {
            (
                "warning",
                if self.image_path.is_none() {
                    "No finished image found. See log.".into()
                } else {
                    if errors || self.split_assessment.blocked {
                        "Dump completed with errors.".into()
                    } else {
                        "Dump completed with warnings. Review the log.".into()
                    }
                },
            )
        } else {
            (
                "completed",
                "Dump completed.".into(),
            )
        };
        self.status = status.into();
        self.message = message;
        self.refresh_files();
        if self.message == "Dump completed with errors." && self.can_refine {
            self.message.push_str(" Minimize errors with the \"Refine Dump\" button.");
        }
        self.revision += 1;
    }
}

#[derive(Default)]
pub struct DumpInner {
    job: Option<DumpJob>,
    child: Option<CommandChild>,
    next_id: u64,
    request: Option<DumpRequest>,
}

#[derive(Default)]
pub struct RedumperDumpState(pub Arc<Mutex<DumpInner>>);

impl RedumperDumpState {
    pub fn active(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .job
            .as_ref()
            .is_some_and(DumpJob::active)
    }

    pub fn reserves(&self, drive: &str) -> bool {
        let inner = self.0.lock().unwrap();
        inner
            .job
            .as_ref()
            .is_some_and(|job| job.active() && same_drive(&job.drive, drive))
    }
}

fn same_drive(a: &str, b: &str) -> bool {
    let key = |s: &str| {
        s.trim_start_matches("/dev/")
            .trim_end_matches(['/', '\\'])
            .to_ascii_lowercase()
    };
    key(a) == key(b)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct DumpOptions {
    any_drive: bool,
    complete_with_errors: bool,
    double_dump: bool,
    correct_offset_shift: bool,
    retries: u32,
    log_archive: ArchiveMode,
    auto_eject: bool,
    skeleton: bool,
    rings: bool,
    verbose: bool,
    refine_subchannel: bool,
    refine_sector_mode: bool,
    dvd_raw: bool,
    bd_raw: bool,
}
impl Default for DumpOptions {
    fn default() -> Self {
        Self {
            any_drive: true,
            complete_with_errors: true,
            double_dump: false,
            correct_offset_shift: true,
            retries: 0,
            log_archive: ArchiveMode::SevenZip,
            auto_eject: false,
            skeleton: true,
            rings: false,
            verbose: false,
            refine_subchannel: false,
            refine_sector_mode: false,
            dvd_raw: false,
            bd_raw: false,
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct DumpRequest {
    drive: String,
    drive_name: String,
    output_parent: String,
    name: String,
    speed: Option<u32>,
    source: String,
    external_path: Option<String>,
    #[serde(default)]
    manual_command: Option<String>,
    #[serde(default)]
    options: DumpOptions,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn validate_name(name: &str) -> Result<(), String> {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .is_some_and(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_digit())
        });
    if name.trim().is_empty()
        || name.starts_with('.')
        || name.ends_with(['.', ' '])
        || name.chars().count() > 100
        || reserved
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return Err("Choose a disc name of up to 100 characters without path separators or reserved filename characters.".into());
    }
    Ok(())
}

fn prepare_output(parent: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    // An existing parent is selected in the folder picker. Canonicalising it also
    // makes the command independent of redumper's working directory.
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("Cannot open output folder: {e}"))?;
    let output = parent.join(name);
    // Atomic create, never merge a new dump into an existing (even empty) folder.
    std::fs::create_dir(&output).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            "Folder already exists. Choose another image name or location.".into()
        } else {
            format!("Cannot create dump folder: {e}")
        }
    })?;
    Ok(output)
}

fn completed_image(output: &Path, name: &str) -> Option<PathBuf> {
    // Prefer the cue sheet: opening one track BIN would lose the disc layout.
    ["cue", "iso"]
        .iter()
        .map(|ext| output.join(format!("{name}.{ext}")))
        .find(|p| p.metadata().is_ok_and(|m| m.is_file() && m.len() > 0))
}

fn needs_review(status: &str) -> bool {
    matches!(status, "warning" | "failed" | "cancelled")
}

fn regular_file_len(path: &Path) -> Option<u64> {
    // Refine writes in place. Do not follow links to files outside this dump.
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.is_file() && m.len() > 0)
        .map(|m| m.len())
}

fn refinable_files(output: &Path, name: &str) -> bool {
    if validate_name(name).is_err() || !std::fs::symlink_metadata(output).is_ok_and(|m| m.is_dir())
    {
        return false;
    }
    let file = |ext: &str| output.join(format!("{name}.{ext}"));
    if regular_file_len(&file("log")).is_none() {
        return false;
    }
    let Some(state_len) = regular_file_len(&file("state")) else {
        return false;
    };
    let [cd, dvd, bd, iso] =
        ["scram", "sdram", "sbram", "iso"].map(|ext| regular_file_len(&file(ext)).is_some());
    if u8::from(cd) + u8::from(dvd) + u8::from(bd) > 1 || (cd && iso) || !(cd || dvd || bd || iso) {
        return false;
    }
    // b760 state layout: 588 sample bytes per CD sector from LBA -45150;
    // one byte per raw DVD/BD sector from -0x30000 / -0x100000.
    let lba_zero = if cd {
        26_548_200
    } else if dvd {
        0x30000
    } else if bd {
        0x100000
    } else {
        0
    };
    if state_len <= lba_zero {
        return false;
    }
    if cd
        && (["toc", "subcode"]
            .iter()
            .any(|ext| regular_file_len(&file(ext)).is_none()))
    {
        return false;
    }
    // The aggregate workflow can replace sibling output files as well. Reject
    // links belonging to this dump, including links to missing destinations.
    let Ok(entries) = std::fs::read_dir(output) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let filename = entry.file_name().to_string_lossy().into_owned();
        if (filename.starts_with(&format!("{name}.")) || filename.starts_with(&format!("{name} (")))
            && !entry.file_type().is_ok_and(|t| t.is_file())
        {
            return false;
        }
    }
    true
}

fn validate_refine_job(job: &DumpJob, id: u64) -> Result<PathBuf, String> {
    if job.id != id || job.active() || !needs_review(&job.status) {
        return Err("This dump is no longer available to refine.".into());
    }
    let output = PathBuf::from(&job.output_path);
    if !refinable_files(&output, &job.name) {
        return Err("Cannot refine: recovery files missing or incompatible.".into());
    }
    Ok(output)
}

fn dump_args(request: &DumpRequest, output: &Path, refine: bool) -> Vec<String> {
    let mut args = vec![
        "disc".into(),
        format!("--drive={}", request.drive),
        format!("--image-path={}", output.display()),
        format!("--image-name={}", request.name),
        "--leave-unchanged".into(),
    ];
    if request.options.any_drive {
        args.push("--drive-type=GENERIC".into());
    }
    if request.options.complete_with_errors {
        args.push("--force-split".into());
    }
    for (enabled, flag) in [
        (
            request.options.correct_offset_shift,
            "--correct-offset-shift",
        ),
        (request.options.skeleton, "--skeleton"),
        (request.options.rings, "--rings"),
        (request.options.verbose, "--verbose"),
        (request.options.refine_subchannel, "--refine-subchannel"),
        (request.options.refine_sector_mode, "--refine-sector-mode"),
        (request.options.dvd_raw, "--dvd-raw"),
        (request.options.bd_raw, "--bd-raw"),
    ] {
        if enabled {
            args.push(flag.into());
        }
    }
    args.push(format!("--retries={}", request.options.retries));
    // The app ejects after the entire workflow. redumper's --auto-eject runs
    // before splitting/hashing and would remove the disc before a second pass.
    if refine {
        // Resume after acquisition, then regenerate split files and hashes.
        // Keep redumper's TOC checks; never use --force-refine.
        args.extend(["--continue=refine".into(), "--overwrite".into()]);
    }
    if let Some(speed) = request.speed {
        args.push(format!("--speed={speed}"));
    }
    args
}

#[derive(Serialize)]
pub struct CommandPreview {
    command: String,
    generated: String,
}

#[tauri::command]
pub fn preview_redumper_command(
    mut request: DumpRequest,
    refine: bool,
    job_id: Option<u64>,
    state: tauri::State<'_, RedumperDumpState>,
) -> Result<CommandPreview, String> {
    let output = if let Some(id) = job_id {
        let inner = state.0.lock().unwrap();
        let job = inner.job.as_ref().filter(|j| j.id == id).ok_or("Dump is no longer available.")?;
        let manual = request.manual_command;
        request = inner.request.clone().ok_or("Original settings are unavailable.")?;
        if manual.is_some() { request.manual_command = manual; }
        PathBuf::from(&job.output_path)
    } else {
        let parent = Path::new(&request.output_parent);
        parent.canonicalize().unwrap_or_else(|_| parent.into()).join(&request.name)
    };
    Ok(CommandPreview {
        command: command::display(&command::arguments(&request, &output, refine)?),
        generated: command::display(&dump_args(&request, &output, refine)),
    })
}

fn number_after(line: &str, marker: &str) -> Option<u64> {
    let tail = line.split_once(marker)?.1.trim_start();
    tail.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

fn parse_progress(line: &str) -> Option<DumpProgress> {
    let tail = line.split_once("LBA:")?.1.trim();
    let (current, rest) = tail.split_once('/')?;
    let current = current.trim().parse::<i64>().ok()?;
    let total = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse::<i64>()
        .ok()?;
    let percentage = (total > 0).then(|| (current as f64 / total as f64 * 100.0).clamp(0.0, 100.0));
    Some(DumpProgress {
        percentage,
        current: Some(current),
        total: Some(total),
        scsi: number_after(line, "SCSI:").or_else(|| number_after(line, "SCSIs:")),
        edc: number_after(line, "EDC:"),
        c2: number_after(line, "C2s:"),
        q: number_after(line, "Q:"),
    })
}

// Raw stdout can split a UTF-8 character or a progress line between events.
// Both CR (in-place progress) and LF terminate a line; streams stay separate.
#[derive(Default)]
struct Lines(Vec<u8>);
impl Lines {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for &byte in bytes {
            if (matches!(byte, b'\r' | b'\n') || self.0.len() >= 65536) && !self.0.is_empty() {
                out.push(String::from_utf8_lossy(&self.0).into_owned());
                self.0.clear();
            }
            if !matches!(byte, b'\r' | b'\n') {
                self.0.push(byte);
            }
        }
        out
    }
    fn finish(&mut self) -> Vec<String> {
        self.push(b"\n")
    }
}

#[tauri::command]
pub fn get_dump_job(state: tauri::State<'_, RedumperDumpState>) -> Option<DumpJob> {
    let mut inner = state.0.lock().unwrap();
    if let Some(job) = inner.job.as_mut() {
        job.refresh_files();
    }
    inner.job.clone()
}

#[tauri::command]
pub async fn start_redumper_dump(
    request: DumpRequest,
    app: tauri::AppHandle,
    state: tauri::State<'_, RedumperDumpState>,
) -> Result<DumpJob, String> {
    run_dump(request, None, app, state).await
}

fn existing_refine_output(parent: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    let output = parent
        .canonicalize()
        .map_err(|e| format!("Cannot open output folder: {e}"))?
        .join(name);
    if !refinable_files(&output, name) {
        return Err("Cannot refine: recovery files missing or incompatible.".into());
    }
    Ok(output)
}

#[tauri::command]
pub fn inspect_redumper_dump(output_parent: String, name: String) -> bool {
    existing_refine_output(Path::new(&output_parent), &name).is_ok()
}

#[tauri::command]
pub async fn refine_existing_redumper_dump(
    request: DumpRequest,
    overwrite_confirmed: bool,
    app: tauri::AppHandle,
    state: tauri::State<'_, RedumperDumpState>,
) -> Result<DumpJob, String> {
    if !overwrite_confirmed {
        return Err("Confirm updating the existing dump files before refining.".into());
    }
    run_dump(request, Some(RefineTarget::Existing), app, state).await
}

#[derive(Clone, Copy)]
enum RefineTarget {
    Job(u64),
    Existing,
}

#[tauri::command]
pub async fn refine_redumper_dump(
    id: u64,
    overwrite_confirmed: bool,
    manual_command: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, RedumperDumpState>,
) -> Result<DumpJob, String> {
    if !overwrite_confirmed {
        return Err("Confirm updating the existing dump files before refining.".into());
    }
    let mut request = {
        let inner = state.0.lock().unwrap();
        let job = inner
            .job
            .as_ref()
            .ok_or("No dump is available to refine.")?;
        validate_refine_job(job, id)?;
        inner
            .request
            .clone()
            .ok_or("The original dump settings are unavailable.")?
    };
    if manual_command.is_some() { request.manual_command = manual_command; }
    run_dump(request, Some(RefineTarget::Job(id)), app, state).await
}

async fn run_dump(
    request: DumpRequest,
    refine_job_id: Option<RefineTarget>,
    app: tauri::AppHandle,
    state: tauri::State<'_, RedumperDumpState>,
) -> Result<DumpJob, String> {
    if request.drive.trim().is_empty() {
        return Err("Select a drive.".into());
    }
    validate_options(&request.options)?;
    if request.speed.is_some_and(|s| s == 0 || s > 72) {
        return Err("Invalid drive speed.".into());
    }
    if !request.options.any_drive {
        let result = super::redumper_cmd(&request.source, request.external_path.as_deref(), &app)?
            .args(["--list-recommended-drives"])
            .output()
            .await
            .map_err(|e| e.to_string())?;
        if !result.status.success()
            || !recommended_drive(
                &request.drive_name,
                &String::from_utf8_lossy(&result.stdout),
            )
        {
            return Err("Drive approval unknown. Choose \"Works with Any Drive Model\" in Settings.".into());
        }
    }
    let mut cmd = super::redumper_cmd(&request.source, request.external_path.as_deref(), &app)?;
    let state = state.0.clone();
    let (rx, job) = {
        let mut inner = state.lock().unwrap();
        if inner.job.as_ref().is_some_and(DumpJob::active) {
            return Err("Dump already running. Stop it to start another.".into());
        }
        let parent = Path::new(&request.output_parent);
        let planned_output = parent.canonicalize().unwrap_or_else(|_| parent.into()).join(&request.name);
        command::arguments(&request, &planned_output, refine_job_id.is_some())?;
        let output = if let Some(RefineTarget::Job(id)) = refine_job_id {
            // Recheck under the same lock used to reserve the drive: neither a
            // different job nor removed files may slip between approval/start.
            validate_refine_job(
                inner
                    .job
                    .as_ref()
                    .ok_or("No dump is available to refine.")?,
                id,
            )?
        } else if matches!(refine_job_id, Some(RefineTarget::Existing)) {
            existing_refine_output(Path::new(&request.output_parent), &request.name)?
        } else {
            prepare_output(Path::new(&request.output_parent), &request.name)?
        };
        let args = command::arguments(&request, &output, refine_job_id.is_some())?;
        let executed_command = command::display(&args);
        cmd = cmd
            .args(args)
            .set_raw_out(true);
        let (rx, child) = match cmd.spawn() {
            Ok(spawned) => spawned,
            Err(e) => {
                if refine_job_id.is_none() {
                    let _ = std::fs::remove_dir(&output); // Only remove our new, empty directory.
                }
                return Err(format!("Could not start redumper: {e}"));
            }
        };
        inner.request = Some(request.clone());
        inner.next_id += 1;
        let job = DumpJob {
            id: inner.next_id,
            revision: 0,
            drive: request.drive.clone(),
            drive_name: request.drive_name.clone(),
            name: request.name.clone(),
            output_path: output.to_string_lossy().into_owned(),
            image_path: None,
            can_refine: false,
            output_exists: true,
            status: "running".into(),
            stage: "Starting".into(),
            message: String::new(),
            command: executed_command,
            started_at: now_ms(),
            finished_at: None,
            progress: DumpProgress::default(),
            logs: vec![],
            warnings: false,
            dvd_or_bd: false,
            pass: 0,
            split_assessment: SplitAssessment::default(),
            read_error_summary: ReadErrorSummary::default(),
        };
        inner.child = Some(child);
        inner.job = Some(job.clone());
        (rx, job)
    };
    let _ = app.emit("dump-job", &job);
    let id = job.id;
    tauri::async_runtime::spawn(async move {
        let mut exit_code = consume_output(rx, &state, &app, id).await;
        let verify = {
            let mut inner = state.lock().unwrap();
            let job = inner.job.as_mut().unwrap();
            job.warnings |= has_errors(&job.progress);
            exit_code == Some(0)
                && job.status == "running"
                && refine_job_id.is_none()
                && request.options.double_dump
                && job.dvd_or_bd
        };
        let verification = if verify {
            match verify_second_dump(&request, &state, &app, id).await {
                Ok(message) => Some(message),
                Err(error) => {
                    exit_code = Some(1);
                    Some(error)
                }
            }
        } else {
            None
        };
        let archive_message = if exit_code == Some(0)
            && request.options.log_archive != ArchiveMode::Off
        {
            match archive_after_dump(&request, &state, &app, id, verify).await {
                Ok(message) => Some(message),
                Err(error) => {
                    if let Some(job) = state
                        .lock()
                        .unwrap()
                        .job
                        .as_mut()
                        .filter(|j| j.id == id && j.status == "running")
                    {
                        job.line(&format!("Warning: log compression failed: {error}. Original files have been kept."));
                    }
                    None
                }
            }
        } else {
            None
        };
        let eject = {
            let inner = state.lock().unwrap();
            inner
                .job
                .as_ref()
                .is_some_and(|job| ready_to_eject(&request.options, job, exit_code))
        };
        if eject {
            if let Err(error) = eject_after_dump(&request, &state, &app, id).await {
                if let Some(job) = state.lock().unwrap().job.as_mut().filter(|j| j.id == id) {
                    job.line(&format!("Warning: automatic eject failed: {error}"));
                }
            }
        }
        let mut inner = state.lock().unwrap();
        if let Some(job) = inner.job.as_mut().filter(|j| j.id == id) {
            job.finish(exit_code);
            if job.status != "cancelled" {
                if let Some(message) = verification {
                    job.line(&message);
                    // A specific comparison failure is more useful than a generic exit-code message.
                    if job.status == "failed" {
                        job.message = message;
                    } else if job.status == "completed" {
                        job.message = format!("{} {}", job.message, message);
                    }
                }
                if let Some(message) = archive_message {
                    job.line(&message);
                    // Archive names, fallback reasons, and cleanup details belong in the log.
                    if job.status == "completed" {
                        job.message = format!("{} {}", job.message, archive::status_summary(&message));
                    }
                }
            }
            let _ = app.emit("dump-job", &*job);
            inner.child = None;
        }
    });
    Ok(job)
}

async fn archive_after_dump(
    request: &DumpRequest,
    state: &Arc<Mutex<DumpInner>>,
    app: &tauri::AppHandle,
    id: u64,
    verification: bool,
) -> Result<String, String> {
    let (output, remove_originals) = {
        let mut inner = state.lock().unwrap();
        let job = running_job(&mut inner, id)?;
        let output = match job.prepare_log_archive() {
            Ok(output) => output,
            Err(message) => return Ok(message.into()),
        };
        let _ = app.emit("dump-job", &*job);
        let remove_originals = !job.warnings
            && !has_errors(&job.progress)
            && completed_image(&output, &job.name).is_some();
        (output, remove_originals)
    };
    let state = state.clone();
    let name = request.name.clone();
    let mode = request.options.log_archive;
    tauri::async_runtime::spawn_blocking(move || {
        let tool = archive::find_7z();
        archive::archive_logs(
            &output,
            &name,
            verification,
            mode,
            tool.as_deref(),
            remove_originals,
            || running_job(&mut state.lock().unwrap(), id).map(|_| ()),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn validate_options(options: &DumpOptions) -> Result<(), String> {
    if options.retries > 10000 {
        return Err("Sector retries must be between 0 and 10000.".into());
    }
    Ok(())
}

fn ready_to_eject(options: &DumpOptions, job: &DumpJob, code: Option<i32>) -> bool {
    options.auto_eject
        && code == Some(0)
        && job.status == "running"
        && !job.warnings
        && !has_errors(&job.progress)
        && completed_image(Path::new(&job.output_path), &job.name).is_some()
}

async fn eject_after_dump(
    request: &DumpRequest,
    state: &Arc<Mutex<DumpInner>>,
    app: &tauri::AppHandle,
    id: u64,
) -> Result<(), String> {
    let rx = {
        let mut inner = state.lock().unwrap();
        let job = running_job(&mut inner, id)?;
        let (rx, child) =
            super::redumper_cmd(&request.source, request.external_path.as_deref(), app)?
                .args(["eject".into(), format!("--drive={}", request.drive)])
                .set_raw_out(true)
                .spawn()
                .map_err(|e| e.to_string())?;
        job.pass = 0;
        job.line("*** Ejecting disc");
        let _ = app.emit("dump-job", &*job);
        inner.child = Some(child);
        rx
    };
    match consume_output(rx, state, app, id).await {
        Some(0) => Ok(()),
        code => Err(format!(
            "redumper returned {code:?}. Use Eject Disc to try again."
        )),
    }
}

fn has_errors(p: &DumpProgress) -> bool {
    [p.scsi, p.edc, p.c2, p.q]
        .into_iter()
        .flatten()
        .any(|n| n > 0)
}

fn double_dump_media(profile: &str) -> bool {
    profile
        .to_ascii_uppercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| part == "DVD" || part == "BD")
}

fn recommended_drive(label: &str, list: &str) -> bool {
    let tokens = |s: &str| {
        s.split_whitespace()
            .map(str::to_ascii_uppercase)
            .collect::<Vec<_>>()
    };
    let label = tokens(label);
    list.lines()
        .filter_map(|line| line.split_once(" - "))
        .any(|(_, model)| {
            let model = tokens(
                model
                    .split(" (revision level:")
                    .next()
                    .unwrap_or(model)
                    .trim(),
            );
            // Linux may supply only the model. Compare complete tokens so similar
            // but unsupported model suffixes do not pass the recommended-drive gate.
            !model.is_empty() && label.windows(model.len()).any(|parts| parts == model)
        })
}

async fn consume_output(
    mut rx: tauri::async_runtime::Receiver<CommandEvent>,
    state: &Arc<Mutex<DumpInner>>,
    app: &tauri::AppHandle,
    id: u64,
) -> Option<i32> {
    let mut stdout = Lines::default();
    let mut stderr = Lines::default();
    let mut last_emit = Instant::now();
    let mut exit_code = None;
    while let Some(event) = rx.recv().await {
        let lines = match event {
            CommandEvent::Stdout(bytes) => stdout.push(&bytes),
            CommandEvent::Stderr(bytes) => stderr.push(&bytes),
            CommandEvent::Error(error) => vec![format!("Error: {error}")],
            CommandEvent::Terminated(status) => {
                exit_code = status.code;
                break;
            }
            _ => continue,
        };
        let mut inner = state.lock().unwrap();
        if let Some(job) = inner.job.as_mut().filter(|j| j.id == id) {
            let old_stage = job.stage.clone();
            for line in lines {
                job.line(&line);
            }
            if job.stage != old_stage || last_emit.elapsed() >= Duration::from_millis(150) {
                let _ = app.emit("dump-job", &*job);
                last_emit = Instant::now();
            }
        }
    }
    let mut inner = state.lock().unwrap();
    if let Some(job) = inner.job.as_mut().filter(|j| j.id == id) {
        for line in stdout.finish().into_iter().chain(stderr.finish()) {
            job.line(&line);
        }
        inner.child = None;
    }
    exit_code
}

fn running_job(inner: &mut DumpInner, id: u64) -> Result<&mut DumpJob, String> {
    inner
        .job
        .as_mut()
        .filter(|j| j.id == id && j.status == "running")
        .ok_or_else(|| "Verification stopped. Both dumps have been kept.".into())
}

// Read full image contents, rather than trusting hashes printed by the dumper.
// The callback permits cancellation between bounded reads, including between files.
fn image_sha256(
    path: &Path,
    mut progress: impl FnMut(u64) -> Result<(), String>,
) -> Result<String, String> {
    if !std::fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Err("Verification requires regular image files.".into());
    }
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut read = 0;
    loop {
        progress(read)?;
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        read += count as u64;
    }
    if read == 0 {
        return Err("Cannot verify an empty image.".into());
    }
    Ok(format!("{:x}", hash.finalize()))
}

async fn verify_second_dump(
    request: &DumpRequest,
    state: &Arc<Mutex<DumpInner>>,
    app: &tauri::AppHandle,
    id: u64,
) -> Result<String, String> {
    let (rx, output, first, second) = {
        let mut inner = state.lock().unwrap();
        let job = running_job(&mut inner, id)?;
        let output = PathBuf::from(&job.output_path);
        let first = output.join(format!("{}.iso", request.name));
        if !first.is_file() {
            return Err("The first dump has no ISO to compare.".into());
        }
        // A separate fresh directory avoids all overwrites and preserves both
        // image sets, including logs/state for troubleshooting a mismatch.
        let verify = prepare_output(&output, "verification")?;
        let second = verify.join(format!("{}.iso", request.name));
        let args = dump_args(request, &verify, false);
        let executed_command = command::display(&args);
        let command = super::redumper_cmd(&request.source, request.external_path.as_deref(), app)?
            .args(args)
            .set_raw_out(true);
        let (rx, child) = command
            .spawn()
            .map_err(|e| format!("Could not start the second dump: {e}"))?;
        job.pass = 2;
        job.command = executed_command;
        job.progress = DumpProgress::default();
        job.line("*** Second dump");
        job.line("Reading the disc again for SHA-256 comparison. Both copies will be kept.");
        let _ = app.emit("dump-job", &*job);
        inner.child = Some(child);
        (rx, output, first, second)
    };
    let exit = consume_output(rx, state, app, id).await;
    {
        let mut inner = state.lock().unwrap();
        let job = running_job(&mut inner, id)?;
        if exit != Some(0) {
            return Err(
                "Second dump failed. Both copies kept; comparison skipped."
                    .into(),
            );
        }
        job.pass = 0;
        job.line("*** Comparing SHA-256 hashes");
        let _ = app.emit("dump-job", &*job);
    }
    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let sizes = [std::fs::metadata(&first), std::fs::metadata(&second)];
        let total = sizes
            .into_iter()
            .try_fold(0u64, |sum, size| size.map(|m| sum.saturating_add(m.len())))
            .map_err(|e| format!("Cannot compare the two images: {e}"))?;
        let mut base = 0;
        let mut hashes = Vec::new();
        let mut last_emit = Instant::now();
        for path in [&first, &second] {
            let hash = image_sha256(path, |bytes| {
                let mut inner = state.lock().unwrap();
                let job = running_job(&mut inner, id)?;
                job.progress.percentage =
                    (total > 0).then(|| ((base + bytes) as f64 / total as f64 * 100.0).min(100.0));
                job.revision += 1;
                if last_emit.elapsed() >= Duration::from_millis(150) {
                    let _ = app.emit("dump-job", &*job);
                    last_emit = Instant::now();
                }
                Ok(())
            })?;
            base += std::fs::metadata(path).map_err(|e| e.to_string())?.len();
            hashes.push(hash);
        }
        let mut inner = state.lock().unwrap();
        let job = running_job(&mut inner, id)?;
        let report = format!(
            "{}  {}\n{}  verification/{}\n",
            hashes[0],
            first.file_name().unwrap().to_string_lossy(),
            hashes[1],
            second.file_name().unwrap().to_string_lossy()
        );
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output.join("verification.sha256"))
            .and_then(|mut f| f.write_all(report.as_bytes()))
            .map_err(|e| format!("Could not save the hash comparison: {e}"))?;
        job.line(&report);
        if hashes[0] != hashes[1] {
            return Err(
                "Hashes differ. Both copies kept.".into(),
            );
        }
        Ok("Hashes match. Both copies kept.".into())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_redumper_dump(
    id: u64,
    app: tauri::AppHandle,
    state: tauri::State<'_, RedumperDumpState>,
) -> Result<(), String> {
    let mut inner = state.0.lock().unwrap();
    let job = inner
        .job
        .as_mut()
        .filter(|j| j.id == id && j.active())
        .ok_or("This dump is no longer running.")?;
    if job.status == "stopping" {
        return Ok(());
    }
    job.status = "stopping".into();
    job.revision += 1;
    let snapshot = job.clone();
    let _ = app.emit("dump-job", snapshot);
    // Keep the job reserved until the termination event, even after kill returns.
    if let Some(child) = inner.child.take() {
        child
            .kill()
            .map_err(|e| format!("Could not stop redumper: {e}"))?;
    }
    Ok(())
}

pub fn prevent_close(app: &tauri::AppHandle) -> bool {
    if app.state::<RedumperDumpState>().active() {
        let _ = app.emit("dump-close-blocked", ());
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> DumpJob {
        DumpJob {
            id: 1,
            revision: 0,
            drive: "disk4".into(),
            drive_name: "Drive".into(),
            name: "Disc".into(),
            output_path: "/nonexistent/disc-dump-test".into(),
            image_path: None,
            can_refine: false,
            output_exists: true,
            status: "running".into(),
            stage: "Starting".into(),
            message: String::new(),
            command: String::new(),
            started_at: 0,
            finished_at: None,
            progress: DumpProgress::default(),
            logs: vec![],
            warnings: false,
            dvd_or_bd: false,
            pass: 0,
            split_assessment: SplitAssessment::default(),
            read_error_summary: ReadErrorSummary::default(),
        }
    }

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
                "dx-refine-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, ext: &str, size: u64) {
            std::fs::File::create(self.0.join(format!("Disc.{ext}")))
                .unwrap()
                .set_len(size)
                .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn final_split_verdict_survives_log_truncation_and_resets_for_a_new_job() {
        let mut j = job();
        for line in [
            "*** SPLIT",
            "checking tracks",
            "errors detected, track: 01, sectors: {SKIP: 2, C2: 0}, samples: {SKIP: 1176, C2: 0}",
            "done",
            "writing tracks",
            "done",
            "*** HASH",
        ] {
            j.line(line);
        }
        for i in 0..500 {
            j.line(&format!("hash/info {i}"));
        }
        assert!(j.split_assessment.blocked);
        assert!(j.warnings);
        assert!(!j.logs.iter().any(|l| l.contains("errors detected, track")));
        assert!(
            !job().split_assessment.blocked,
            "new dump/refine jobs start without the old verdict"
        );
        let mut allowed = job();
        for line in [
            "*** SPLIT",
            "checking tracks",
            "errors detected, track: 01, sectors: {SKIP: 0, C2: 2}, samples: {SKIP: 0, C2: 1176}",
            "warning: CD-R trailing C2 errors detected (sectors: 2)",
            "done",
            "*** HASH",
        ] {
            allowed.line(line);
        }
        assert!(allowed.warnings, "general warnings remain visible");
        assert!(
            !allowed.split_assessment.blocked,
            "allowed boundaries do not block compression"
        );
        assert!(allowed.prepare_log_archive().is_ok());
    }

    #[test]
    fn unresolved_track_errors_defer_compression_until_clean_refinement() {
        let f = Fixture::new();
        for (ext, size) in [
            ("scram", 2352),
            ("state", 45150 * 588 + 1),
            ("toc", 1),
            ("subcode", 1),
            ("log", 1),
            ("cue", 1),
        ] {
            f.file(ext, size);
        }
        let mut j = job();
        j.output_path = f.0.to_string_lossy().into_owned();
        for line in [
            "*** SPLIT",
            "checking tracks",
            "errors detected, track: 01, sectors: {SKIP: 1, C2: 0}, samples: {SKIP: 588, C2: 0}",
            "done",
            "*** HASH",
        ] {
            j.line(line);
        }
        let message = j.prepare_log_archive().unwrap_err();
        assert!(message.contains("compression skipped"));
        assert_eq!(j.stage, "HASH");
        j.finish(Some(0));
        assert_eq!(j.status, "warning");
        assert!(j.can_refine);
        assert_eq!(std::fs::read_dir(&f.0).unwrap().count(), 6);

        // A refinement gets a fresh job and independently assesses remaining errors.
        let mut refined = job();
        refined.output_path = j.output_path;
        for line in ["*** SPLIT", "checking tracks", "done", "*** HASH"] {
            refined.line(line);
        }
        assert_eq!(refined.prepare_log_archive().unwrap(), f.0);
        assert_eq!(refined.stage, "Compressing logs");
    }

    #[test]
    fn existing_dump_discovery_rechecks_recovery_files_without_a_session() {
        let root = Fixture::new();
        let saved = Fixture(root.0.join("Disc"));
        std::fs::create_dir(&saved.0).unwrap();
        for (ext, size) in [
            ("scram", 2352),
            ("state", 26_548_201),
            ("toc", 20),
            ("subcode", 96),
            ("log", 1),
        ] {
            saved.file(ext, size);
        }
        assert_eq!(existing_refine_output(&root.0, "Disc").unwrap(), saved.0.canonicalize().unwrap());
        assert!(inspect_redumper_dump(
            root.0.to_string_lossy().into_owned(),
            "Disc".into()
        ));
        assert!(existing_refine_output(&root.0, "../Disc").is_err());
        std::fs::remove_file(saved.0.join("Disc.state")).unwrap();
        assert!(existing_refine_output(&root.0, "Disc").is_err());
        assert!(!inspect_redumper_dump(
            root.0.to_string_lossy().into_owned(),
            "Disc".into()
        ));
        assert!(saved.0.join("Disc.scram").exists());
    }

    #[test]
    #[ignore]
    fn existing_dump_discovery_real_folder() {
        let path = PathBuf::from(std::env::var("DX_DUMP_FOLDER").expect("set DX_DUMP_FOLDER"));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(inspect_redumper_dump(
            path.parent().unwrap().to_string_lossy().into_owned(),
            name.into()
        ));
        println!("Recoverable dump found: {}", path.display());
    }

    #[test]
    fn final_correction_totals_replace_stale_progress_and_preserve_real_errors() {
        let f = Fixture::new();
        f.file("iso", 2048);
        for (corrected, remaining, status) in [(553152, 0, "completed"), (553145, 7, "warning")] {
            let mut j = job();
            j.output_path = f.0.to_string_lossy().into_owned();
            for line in [
                "profile: DVD-ROM",
                "*** REFINE (time check: 0s)",
                "analyzing dump... done",
                "[22%] LBA: 164416/717568, errors: { SCSI: 553152, EDC: 0 }",
                "[99%] LBA: 717536/717568, errors: { SCSI: 32, EDC: 0 }",
                "correction statistics:",
                &format!("  SCSI: {corrected}"),
                "  EDC: 0",
                "*** HASH (time check: 0s)",
                "*** END (time check: 15s)",
            ] {
                j.line(line);
            }
            assert_eq!(j.progress.scsi, Some(remaining));
            assert_eq!(j.progress.edc, Some(0));
            assert_eq!(j.prepare_log_archive().is_ok(), remaining == 0);
            j.finish(Some(0));
            assert_eq!(j.status, status);
        }
    }

    #[test]
    fn error_summaries_handle_cd_units_multiple_passes_and_incomplete_runs() {
        let mut j = job();
        for line in [
            "*** DUMP",
            "media errors:",
            "SCSI: 1176 samples",
            "C2: 588 samples",
            "Q: 2",
            "*** REFINE",
            "LBA: -150/200, errors: { SCSIs: 1176, C2s: 588, Q: 2 }",
            "LBA: 199/200, errors: { SCSIs: 588, C2s: 588, Q: 1 }",
            "correction statistics:",
            "SCSI: 1176 samples",
            "C2: 588 samples",
            "Q: 1 sectors",
        ] {
            j.line(line);
        }
        assert_eq!(
            (j.progress.scsi, j.progress.c2, j.progress.q),
            (Some(0), Some(0), Some(1))
        );
        for line in [
            "*** REFINE",
            "LBA: 199/200, errors: { SCSIs: 0, C2s: 0, Q: 1 }",
            "correction statistics:",
            "SCSI: 0 samples",
            "C2: 0 samples",
            "Q: 1 sectors",
        ] {
            j.line(line);
        }
        assert!(!has_errors(&j.progress));
        j.line("*** HASH");
        j.line("SCSI: 999"); // Outside a summary, never reinterpret arbitrary log text.
        assert_eq!(j.progress.scsi, Some(0));
        j.line("*** REFINE");
        j.line("LBA: 100/200, errors: { SCSIs: 32, C2s: 0, Q: 0 }");
        j.finish(Some(1));
        assert_eq!(j.progress.scsi, Some(32));
        assert_eq!(j.status, "failed");
    }

    #[test]
    fn settings_defaults_and_independent_switches_map_to_commands() {
        let mut request: DumpRequest = serde_json::from_value(serde_json::json!({
            "drive": "disk4", "drive_name": "Drive", "output_parent": "/dumps",
            "name": "Disc", "source": "internal", "speed": null, "external_path": null
        }))
        .unwrap();
        assert!(request.options.any_drive);
        assert!(request.options.complete_with_errors);
        assert!(!request.options.double_dump);
        assert!(request.options.correct_offset_shift);
        assert_eq!(request.options.retries, 0);
        assert!(!request.options.auto_eject && request.options.skeleton && !request.options.rings);
        for any_drive in [true, false] {
            for complete in [true, false] {
                request.options.any_drive = any_drive;
                request.options.complete_with_errors = complete;
                for refine in [false, true] {
                    let args = dump_args(&request, Path::new("/dumps/Disc"), refine);
                    assert_eq!(
                        args[0], "disc",
                        "split/hash must run for error policy to apply"
                    );
                    assert_eq!(args.contains(&"--drive-type=GENERIC".into()), any_drive);
                    assert_eq!(args.contains(&"--force-split".into()), complete);
                    assert_eq!(args.contains(&"--overwrite".into()), refine);
                }
            }
        }
        let options: DumpOptions = serde_json::from_str(r#"{"double_dump":true}"#).unwrap();
        assert!(options.double_dump && options.any_drive && options.complete_with_errors);
    }

    #[test]
    fn extended_options_reach_every_dump_and_refine_command_without_early_eject() {
        let mut request: DumpRequest = serde_json::from_value(serde_json::json!({
            "drive": "disk4", "drive_name": "Drive", "output_parent": "/dumps",
            "name": "Disc", "source": "internal", "speed": null, "external_path": null
        }))
        .unwrap();
        let default_args = dump_args(&request, Path::new("/dumps/Disc"), false);
        assert!(default_args.contains(&"--correct-offset-shift".into()));
        assert!(default_args.contains(&"--retries=0".into()));
        request.options = serde_json::from_value(serde_json::json!({
            "correct_offset_shift": false, "retries": 7, "auto_eject": true,
            "skeleton": true, "rings": true, "verbose": true,
            "refine_subchannel": true, "refine_sector_mode": true,
            "dvd_raw": true, "bd_raw": true
        }))
        .unwrap();
        for refine in [false, true] {
            for output in ["/dumps/Disc", "/dumps/Disc/verification"] {
                let args = dump_args(&request, Path::new(output), refine);
                for flag in [
                    "--skeleton",
                    "--rings",
                    "--verbose",
                    "--refine-subchannel",
                    "--refine-sector-mode",
                    "--dvd-raw",
                    "--bd-raw",
                    "--retries=7",
                ] {
                    assert!(args.contains(&flag.into()), "{flag}");
                }
                assert!(
                    !args.contains(&"--auto-eject".into()),
                    "never eject between passes"
                );
                assert!(
                    !args.contains(&"--correct-offset-shift".into()),
                    "explicit opt-out persists"
                );
            }
        }
        assert!(validate_options(&request.options).is_ok());
        request.options.retries = 10001;
        assert!(validate_options(&request.options).is_err());
    }

    #[test]
    fn auto_eject_requires_clean_completion_and_is_skipped_after_stop_or_failed_verification() {
        let f = Fixture::new();
        f.file("iso", 2048);
        let mut j = job();
        j.output_path = f.0.to_string_lossy().into_owned();
        let mut options = DumpOptions::default();
        assert!(!ready_to_eject(&options, &j, Some(0)));
        options.auto_eject = true;
        assert!(ready_to_eject(&options, &j, Some(0)));
        assert!(
            !ready_to_eject(&options, &j, Some(1)),
            "verification failure"
        );
        assert!(!ready_to_eject(&options, &j, None));
        j.status = "stopping".into();
        assert!(!ready_to_eject(&options, &j, Some(0)));
        j.status = "running".into();
        j.warnings = true;
        assert!(!ready_to_eject(&options, &j, Some(0)));
        j.warnings = false;
        j.progress.edc = Some(1);
        assert!(!ready_to_eject(&options, &j, Some(0)));
        j.progress.edc = Some(0);
        std::fs::remove_file(f.0.join("Disc.iso")).unwrap();
        assert!(!ready_to_eject(&options, &j, Some(0)));
    }

    #[test]
    fn supported_media_and_recommended_models_are_matched_precisely() {
        for profile in ["DVD-ROM", "DVD+R DL", "BD-ROM", "BD-RE", "PS3 BD-ROM"] {
            assert!(double_dump_media(profile), "{profile}");
        }
        for profile in ["CD-ROM", "CD-R", "HD DVDish", "HDDVD-ROM", ""] {
            assert!(!double_dump_media(profile), "{profile}");
        }
        let list =
            "header\nHL-DT-ST - BD-RE BU40N (revision level: 1.00, vendor specific: <empty>)\n";
        assert!(recommended_drive("HL-DT-ST BD-RE BU40N", list));
        assert!(recommended_drive("bd-re bu40n", list));
        assert!(!recommended_drive("HL-DT-ST BD-RE BU40NX", list));
        assert!(!recommended_drive("HL-DT-ST BD-RE BU40", list));
        assert!(!recommended_drive("HL-DT-ST BD-RE BU40N", ""));
    }

    #[test]
    fn refresh_clears_deleted_files_but_never_disturbs_an_active_job() {
        let f = Fixture::new();
        for (ext, len) in [("iso", 2048), ("state", 1), ("log", 1)] {
            f.file(ext, len);
        }
        let mut j = job();
        j.output_path = f.0.to_string_lossy().into_owned();
        j.status = "stopping".into();
        j.finish(None);
        assert!(j.output_exists && j.can_refine && j.image_path.is_some());
        let revision = j.revision;
        std::fs::remove_dir_all(&f.0).unwrap();
        j.refresh_files();
        assert!(!j.output_exists && !j.can_refine && j.image_path.is_none());
        assert!(j.revision > revision);
        let revision = j.revision;
        j.refresh_files();
        assert_eq!(j.revision, revision);
        j.status = "running".into();
        j.can_refine = true; // Refresh must not mutate active job state.
        j.refresh_files();
        assert!(j.can_refine);
        assert_eq!(j.revision, revision);
    }

    #[test]
    fn hashes_read_contents_and_can_stop_between_chunks_and_passes() {
        let f = Fixture::new();
        let first = f.0.join("Disc.iso");
        std::fs::write(&first, b"abc").unwrap();
        assert_eq!(
            image_sha256(&first, |_| Ok(())).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let verification = prepare_output(&f.0, "verification").unwrap();
        let second = verification.join("Disc.iso");
        std::fs::write(&second, b"abd").unwrap();
        assert_ne!(
            image_sha256(&first, |_| Ok(())).unwrap(),
            image_sha256(&second, |_| Ok(())).unwrap()
        );
        assert!(prepare_output(&f.0, "verification").is_err());
        assert_eq!(std::fs::read(&first).unwrap(), b"abc");
        std::fs::write(&first, vec![0; 2 * 1024 * 1024]).unwrap();
        let mut calls = 0;
        assert!(image_sha256(&first, |_| {
            calls += 1;
            if calls == 2 {
                Err("Stopped".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(calls, 2);
        let mut inner = DumpInner {
            job: Some(job()),
            ..Default::default()
        };
        assert!(running_job(&mut inner, 1).is_ok());
        inner.job.as_mut().unwrap().status = "stopping".into();
        assert!(
            running_job(&mut inner, 1).is_err(),
            "must not start pass two or finish hashing after cancellation"
        );
        std::fs::write(&second, b"").unwrap();
        assert!(image_sha256(&second, |_| Ok(())).is_err());
    }

    #[test]
    fn refine_requires_matching_nonempty_state_log_and_primary_image() {
        let f = Fixture::new();
        f.file("iso", 2048);
        assert!(!refinable_files(&f.0, "Disc"));
        f.file("state", 1);
        assert!(!refinable_files(&f.0, "Disc"));
        f.file("log", 1);
        assert!(refinable_files(&f.0, "Disc"));
        assert!(!refinable_files(&f.0, "Other Disc"));
        assert!(!refinable_files(&f.0, "../Disc"));
        f.file("state", 0);
        assert!(!refinable_files(&f.0, "Disc"));
    }

    #[test]
    fn refine_validates_raw_media_state_offsets_and_cd_sidecars() {
        for (ext, offset) in [
            ("scram", 26_548_200),
            ("sdram", 0x30000),
            ("sbram", 0x100000),
        ] {
            let f = Fixture::new();
            f.file(ext, 1);
            f.file("log", 1);
            f.file("state", offset);
            f.file("toc", 1);
            f.file("subcode", 1);
            assert!(
                !refinable_files(&f.0, "Disc"),
                "{ext}: state must extend past LBA 0"
            );
            f.file("state", offset + 1);
            assert!(refinable_files(&f.0, "Disc"), "{ext}");
            if ext == "scram" {
                std::fs::remove_file(f.0.join("Disc.toc")).unwrap();
                assert!(!refinable_files(&f.0, "Disc"));
                f.file("toc", 1);
                std::fs::remove_file(f.0.join("Disc.subcode")).unwrap();
                assert!(!refinable_files(&f.0, "Disc"));
                f.file("subcode", 1);
                f.file("iso", 2048);
                assert!(!refinable_files(&f.0, "Disc"), "CD and ISO are ambiguous");
            } else {
                f.file("iso", 2048);
                assert!(
                    refinable_files(&f.0, "Disc"),
                    "raw DVD/BD may have a derived ISO"
                );
                f.file(if ext == "sdram" { "sbram" } else { "sdram" }, 1);
                assert!(
                    !refinable_files(&f.0, "Disc"),
                    "multiple raw formats are ambiguous"
                );
            }
        }
    }

    #[test]
    fn refine_is_offered_only_for_reviewable_jobs_and_rechecked_before_launch() {
        let f = Fixture::new();
        for (ext, len) in [("iso", 2048), ("state", 1), ("log", 1)] {
            f.file(ext, len);
        }
        let mut j = job();
        j.output_path = f.0.to_string_lossy().into_owned();
        j.finish(Some(0));
        assert!(!j.can_refine);
        assert!(validate_refine_job(&j, j.id).is_err());
        j.status = "running".into();
        j.progress.scsi = Some(1);
        j.finish(Some(0));
        assert!(j.can_refine);
        assert!(validate_refine_job(&j, j.id).is_ok());
        assert!(validate_refine_job(&j, j.id + 1).is_err());
        j.status = "stopping".into();
        assert!(validate_refine_job(&j, j.id).is_err());
        j.finish(None);
        assert!(j.can_refine);
        std::fs::remove_file(f.0.join("Disc.state")).unwrap();
        assert!(validate_refine_job(&j, j.id).is_err());
        j.status = "running".into();
        j.finish(Some(1));
        assert!(!j.can_refine, "errors alone cannot enable refinement");
    }

    #[test]
    fn refine_resumes_without_acquisition_and_preserves_disc_checks() {
        let request = DumpRequest {
            drive: "disk4".into(),
            drive_name: "Drive".into(),
            output_parent: "/dumps".into(),
            name: "Disc".into(),
            speed: Some(4),
            source: "internal".into(),
            external_path: None,
            manual_command: None,
            options: DumpOptions::default(),
        };
        let args = dump_args(&request, Path::new("/dumps/Disc"), true);
        assert_eq!(args[0], "disc");
        for flag in [
            "--continue=refine",
            "--overwrite",
            "--drive=disk4",
            "--speed=4",
            "--image-path=/dumps/Disc",
        ] {
            assert!(args.iter().any(|a| a == flag), "{flag}");
        }
        assert!(!args.iter().any(|a| a == "--force-refine" || a == "dump"));
        let args = dump_args(&request, Path::new("/dumps/Disc"), false);
        assert_eq!(args[0], "disc");
        assert!(!args
            .iter()
            .any(|a| a == "--overwrite" || a == "--continue=refine"));
    }

    #[cfg(unix)]
    #[test]
    fn refine_rejects_linked_inputs_and_derived_outputs() {
        let f = Fixture::new();
        for (ext, len) in [("iso", 2048), ("state", 1), ("log", 1)] {
            f.file(ext, len);
        }
        std::os::unix::fs::symlink("missing-target", f.0.join("Disc.cue")).unwrap();
        assert!(!refinable_files(&f.0, "Disc"));
        std::fs::remove_file(f.0.join("Disc.cue")).unwrap();
        std::fs::remove_file(f.0.join("Disc.state")).unwrap();
        std::os::unix::fs::symlink("Disc.log", f.0.join("Disc.state")).unwrap();
        assert!(!refinable_files(&f.0, "Disc"));
    }

    #[test]
    fn progress_handles_cd_dvd_negative_lbas_and_unknown_totals() {
        let p = parse_progress("| [50%] LBA: 100/200, errors: { SCSIs: 1, C2s: 2, Q: 3 }").unwrap();
        assert_eq!(p.percentage, Some(50.0));
        assert_eq!(
            (p.scsi, p.c2, p.q, p.edc),
            (Some(1), Some(2), Some(3), None)
        );
        let p = parse_progress("- [0%] LBA: -150/200, errors: { SCSI: 0, EDC: 4 }").unwrap();
        assert_eq!(p.percentage, Some(0.0));
        assert_eq!(p.edc, Some(4));
        assert_eq!(parse_progress("LBA: 10/0").unwrap().percentage, None);
        assert!(parse_progress("track 1/12").is_none());
    }

    #[test]
    fn progress_is_per_stage_and_logs_are_bounded() {
        let mut j = job();
        j.line("LBA: 200/200, errors: { EDC: 4 }");
        j.line("*** REFINE (1)");
        assert_eq!(j.progress.percentage, None);
        j.line("LBA: 10/200, errors: { EDC: 0 }");
        assert_eq!(j.progress.percentage, Some(5.0));
        assert_eq!(j.progress.edc, Some(0));
        j.line("LBA: 20/200, errors: { EDC: 0 }");
        assert_eq!(j.logs.len(), 3);
        for i in 0..1000 {
            j.line(&format!("log {i}"));
        }
        assert_eq!(j.logs.len(), 400);
        j.line("*** SPLIT (time check: 1s)");
        assert_eq!(j.stage, "Generating .bin files");
        j.line("profile: DVD-ROM");
        j.pass = 2;
        j.line("*** SPLIT (time check: 1s)");
        assert_eq!(j.stage, "Dump 2 · Generating .iso file");
    }

    #[test]
    fn fragmented_output_keeps_utf8_and_carriage_return_progress() {
        let mut lines = Lines::default();
        assert!(lines.push(&[b'D', 0xc3]).is_empty());
        assert_eq!(
            lines.push(&[0xa9, b'\r', b'\n', b'1', b'\r', b'2']),
            ["Dé", "1"]
        );
        assert_eq!(lines.finish(), ["2"]);
    }

    #[test]
    fn stop_is_distinct_from_failure_and_missing_output_is_not_success() {
        let mut j = job();
        j.status = "stopping".into();
        assert!(j.active());
        j.finish(None);
        assert_eq!(j.status, "cancelled");
        assert_eq!(j.message, "Dump stopped. Partial files kept.");
        assert!(!j.active());
        let mut j = job();
        j.finish(Some(1));
        assert_eq!(j.status, "failed");
        let mut j = job();
        j.finish(Some(0));
        assert_eq!(j.status, "warning");
    }

    #[test]
    fn output_never_overwrites_and_prefers_cue() {
        let parent =
            std::env::temp_dir().join(format!("dx-dump-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir(&parent).unwrap();
        let output = prepare_output(&parent, "Disc").unwrap();
        std::fs::write(output.join("Disc.iso"), b"image").unwrap();
        assert!(prepare_output(&parent, "Disc").is_err());
        assert_eq!(std::fs::read(output.join("Disc.iso")).unwrap(), b"image");
        std::fs::write(output.join("Disc.cue"), b"cue").unwrap();
        assert_eq!(
            completed_image(&output, "Disc"),
            Some(output.join("Disc.cue"))
        );
        let mut j = job();
        j.output_path = output.to_string_lossy().into_owned();
        j.finish(Some(0));
        assert_eq!(j.status, "completed");
        j.status = "running".into();
        j.progress.edc = Some(1);
        j.finish(Some(0));
        assert_eq!(j.status, "warning");
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn names_cannot_escape_the_destination_on_any_platform() {
        for name in [
            "",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            "C:disc",
            "CON",
            "nul.txt",
            "LPT1",
            "disc.",
            "disc ",
            "a\n",
        ] {
            assert!(validate_name(name).is_err(), "{name}");
        }
        assert!(validate_name("The Disc (日本語)").is_ok());
    }

    #[test]
    fn drive_reservation_handles_os_path_spellings() {
        assert!(same_drive("disk4", "/dev/disk4"));
        assert!(same_drive("D:", "d:\\"));
        assert!(same_drive("/dev/sr0", "/dev/sr0"));
        assert!(!same_drive("disk4", "disk5"));
        let state = RedumperDumpState::default();
        state.0.lock().unwrap().job = Some(job());
        assert!(state.reserves("/dev/disk4"));
        state.0.lock().unwrap().job.as_mut().unwrap().finish(None);
        assert!(!state.reserves("/dev/disk4"));
    }
}
