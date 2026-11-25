//! Interpret redumper's final CD split check, independently of read-time counters.
//! b760 cd/cd_split.ixx::check_tracks filters protection/skip ranges before logging
//! track totals. CD-R warnings then account for allowed boundary C2 sectors.
//! These are the same totals and exceptions used to block splitting without
//! --force-split. Do not treat all warnings or acquisition errors as this verdict.
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::Path;

use super::{cdr, intentional, parse_progress, regular_file_len, DumpProgress, ReadErrorSummary};

pub const PREGAP_WARNING_MESSAGE: &str = "Data track verified. Pregap incomplete.";
pub const SUBCHANNEL_REFINE_MESSAGE: &str = "Data track verified. Refine subchannels.";
const RECORD: &str = ".disc-xplorer-dump.json";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub enum ExpectedErrors {
    #[default]
    None,
    CdrEnd,
    Intentional,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum End {
    Complete,
    Cancelled,
    Failed { message: String, resumable: bool },
    Incomplete,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum OperationWarning {
    Archive,
    ScramCleanup,
    Eject,
    AudioExport,
    AudioCleanup,
}

impl OperationWarning {
    fn message(self) -> &'static str {
        match self {
            Self::Archive => "Could not compress logs.",
            Self::ScramCleanup => "Could not delete .scram.",
            Self::Eject => "Could not eject disc.",
            Self::AudioExport => "Could not export audio.",
            Self::AudioCleanup => "Could not remove the BIN/CUE image.",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Outcome {
    pub end: End,
    pub track_errors: bool,
    pub tracks_verified: bool,
    pub q: Option<u64>,
    pub pregap: bool,
    pub warnings: bool,
    pub expected: ExpectedErrors,
    pub operations: Vec<OperationWarning>,
}

pub struct Presentation {
    pub status: &'static str,
    pub message: String,
    pub can_refine: bool,
    pub auxiliary_warning: bool,
}

impl Outcome {
    pub fn from_evidence(
        end: End,
        info: &FinalInfo,
        split: &SplitAssessment,
        progress: &DumpProgress,
        pregap: bool,
        warnings: bool,
        expected: ExpectedErrors,
    ) -> Self {
        let cdr_end = matches!(expected, ExpectedErrors::CdrEnd);
        let waived = !matches!(expected, ExpectedErrors::None);
        let tracks_verified = !info.errors
            && (info.clean_track_check(split) || (waived && (!split.blocked || cdr_end)));
        Self {
            end,
            track_errors: info.errors
                || (!cdr_end && split.blocked)
                || (!tracks_verified && super::has_track_read_errors(progress)),
            tracks_verified,
            q: progress.q,
            pregap,
            warnings: warnings || (info.layout_anomalies && !waived),
            expected,
            operations: Vec::new(),
        }
    }

    pub fn clean(&self, refine_subchannel: bool) -> bool {
        matches!(self.end, End::Complete)
            && !self.track_errors
            && !self.pregap
            && !self.warnings
            && !self.subchannel_errors(refine_subchannel)
    }

    pub fn subchannel_errors(&self, enabled: bool) -> bool {
        enabled && self.q.is_some_and(|q| q > 0)
    }

    pub fn present(&self, image: bool, recovery: bool, refine_subchannel: bool) -> Presentation {
        let subchannel = self.subchannel_errors(refine_subchannel);
        let incomplete = matches!(
            self.end,
            End::Cancelled
                | End::Incomplete
                | End::Failed {
                    resumable: true,
                    ..
                }
        ) || (!image && matches!(self.end, End::Complete));
        let can_refine = recovery
            && (incomplete
                || self.track_errors
                || (matches!(self.end, End::Complete) && subchannel));
        let (status, mut message) = match &self.end {
            End::Cancelled => ("cancelled", "Dump stopped. Partial files kept.".into()),
            End::Failed { message, .. } => ("failed", message.clone()),
            End::Incomplete => (
                "warning",
                if can_refine {
                    "Existing dump found. Refine to continue."
                } else {
                    "No finished image found. See log."
                }
                .into(),
            ),
            End::Unknown => (
                "unknown",
                if image {
                    "Existing image found.".into()
                } else {
                    String::new()
                },
            ),
            End::Complete if !image => ("warning", "No finished image found. See log.".into()),
            End::Complete if self.track_errors => (
                "warning",
                if can_refine {
                    "Dump completed with errors. Minimize errors with the \"Refine Dump\" button."
                } else {
                    "Dump completed with errors."
                }
                .into(),
            ),
            End::Complete if subchannel && can_refine && self.tracks_verified => {
                ("warning", SUBCHANNEL_REFINE_MESSAGE.into())
            }
            End::Complete if self.pregap && self.tracks_verified => {
                ("warning", PREGAP_WARNING_MESSAGE.into())
            }
            End::Complete if self.warnings || self.pregap || subchannel => (
                "warning",
                "Dump completed with warnings. Review the log.".into(),
            ),
            End::Complete => (
                "completed",
                match self.expected {
                    ExpectedErrors::CdrEnd => cdr::GOOD_MESSAGE,
                    ExpectedErrors::Intentional => intentional::GOOD_MESSAGE,
                    ExpectedErrors::None => "Dump completed.",
                }
                .into(),
            ),
        };
        for operation in &self.operations {
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(operation.message());
        }
        Presentation {
            status,
            message,
            can_refine,
            auxiliary_warning: !self.operations.is_empty(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FinalInfo {
    reports: u32,
    pub errors: bool,
    pub layout_anomalies: bool,
}

impl FinalInfo {
    pub fn line(&mut self, line: &str) {
        if let Some(value) = line.strip_prefix("REDUMP.INFO errors:") {
            if let Ok(count) = value.trim().parse::<u64>() {
                self.reports = self.reports.saturating_add(1);
                self.errors |= count != 0;
            }
        }
        self.layout_anomalies |= ["invalid sync sectors:", "invalid mode sectors:"]
            .iter()
            .any(|label| {
                line.starts_with(label) && super::number_after(line, label).is_some_and(|n| n > 0)
            });
    }

    pub fn clean_track_check(&self, split: &SplitAssessment) -> bool {
        self.reports != 0
            && !self.errors
            && !self.layout_anomalies
            && split.checked
            && !split.blocked
    }
}

pub fn is_pregap_warning(line: &str) -> bool {
    line.starts_with("warning: incomplete pre-gap (session: 1, unavailable: ")
}

pub struct LogEvidence {
    pub end: End,
    pub split: SplitAssessment,
    pub info: FinalInfo,
    pub progress: DumpProgress,
    pub pregap: bool,
    pub warnings: bool,
    pub other_than_cdr_warnings: bool,
    pub other_than_intentional_warnings: bool,
}

pub fn read_log(output: &Path, name: &str) -> Option<LogEvidence> {
    let path = output.join(format!("{name}.log"));
    regular_file_len(&path).filter(|&n| n <= 16 * 1024 * 1024)?;
    let log = fs::read_to_string(path).ok()?;
    let latest = log.rsplit("\n=== ").next()?;
    if !latest
        .lines()
        .any(|line| line.starts_with("redumper (build:"))
    {
        return None;
    }
    let end = if latest
        .trim_end()
        .lines()
        .last()
        .is_some_and(|line| line.starts_with("*** END "))
    {
        End::Complete
    } else {
        End::Incomplete
    };
    let mut split = SplitAssessment::default();
    let mut info = FinalInfo::default();
    let mut progress = DumpProgress::default();
    let mut summary = ReadErrorSummary::default();
    let mut pregap = false;
    let mut warnings = false;
    let mut other_than_cdr_warnings = false;
    let mut other_than_intentional_warnings = false;
    for line in latest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        split.line(line);
        info.line(line);
        let live = parse_progress(line);
        if let Some(ref live) = live {
            progress.merge(live);
        }
        summary.line(line, live.as_ref(), &mut progress);
        let lower = line.to_ascii_lowercase();
        if is_pregap_warning(&lower) {
            pregap = true;
        } else if lower.starts_with("warning:") || lower.starts_with("error:") {
            warnings = true;
            other_than_intentional_warnings |=
                !lower.starts_with("warning: descramble failed (lba: ");
            other_than_cdr_warnings |= !lower
                .starts_with("warning: cd-r trailing c2 errors detected (sectors:")
                && !lower
                    .starts_with("warning: toc / qtoc mismatch, track length (track: 1, lba: ")
                && !lower
                    .starts_with("warning: toc / qtoc mismatch, track index 00 (track: a, lba: ")
                && !lower
                    .starts_with("warning: toc / qtoc mismatch, track index 01 (track: a, lba: ")
                && lower != "warning: toc / qtoc mismatch, nonexistent track in qtoc (track: a)";
        }
    }
    Some(LogEvidence {
        end,
        split,
        info,
        progress,
        pregap,
        warnings,
        other_than_cdr_warnings,
        other_than_intentional_warnings,
    })
}

impl LogEvidence {
    pub fn warnings_for(&self, expected: ExpectedErrors) -> bool {
        match expected {
            ExpectedErrors::CdrEnd => self.other_than_cdr_warnings,
            ExpectedErrors::Intentional => self.other_than_intentional_warnings,
            ExpectedErrors::None => self.warnings,
        }
    }
}

pub fn expected_errors(
    output: &Path,
    name: &str,
    info_errors: bool,
    blocked: bool,
) -> ExpectedErrors {
    if info_errors {
        ExpectedErrors::None
    } else if !blocked && intentional::verified_errors(output, name) {
        ExpectedErrors::Intentional
    } else if cdr::verified_end_errors(output, name) {
        ExpectedErrors::CdrEnd
    } else {
        ExpectedErrors::None
    }
}

#[derive(Debug, Eq, PartialEq, Deserialize, Serialize)]
struct ImageStamp {
    name: String,
    len: u64,
    modified: std::time::SystemTime,
}

// Match the descriptor and all referenced tracks. A same-named replacement,
// incomplete CUE, or missing BIN cannot inherit the prior run's result.
fn image_stamps(image: &Path) -> Option<Vec<ImageStamp>> {
    let stamp = |path: &Path| {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.is_file() || meta.len() == 0 {
            return None;
        }
        Some(ImageStamp {
            name: path.file_name()?.to_str()?.into(),
            len: meta.len(),
            modified: meta.modified().ok()?,
        })
    };
    let mut stamps = vec![stamp(image)?];
    if image.extension()?.eq_ignore_ascii_case("cue") {
        if stamps[0].len > 1024 * 1024 {
            return None;
        }
        let cue = fs::read_to_string(image).ok()?;
        for line in cue
            .lines()
            .map(str::trim_start)
            .filter(|line| line.starts_with("FILE "))
        {
            let name = super::super::extract_quoted(line)?;
            if name.contains(['/', '\\']) || !name.to_ascii_lowercase().ends_with(".bin") {
                return None;
            }
            stamps.push(stamp(&image.with_file_name(name))?);
        }
        if stamps.len() == 1 {
            return None;
        }
    }
    Some(stamps)
}

pub fn valid_image(image: &Path) -> bool {
    image_stamps(image).is_some()
}

#[derive(Deserialize, Serialize)]
struct Record {
    version: u8,
    image: Vec<ImageStamp>,
    log_stamp: Option<(u64, std::time::SystemTime)>,
    outcome: Outcome,
}

fn log_stamp(image: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = fs::symlink_metadata(image.with_extension("log")).ok()?;
    meta.is_file()
        .then_some((meta.len(), meta.modified().ok()?))
}

fn read_record(path: &Path) -> Option<Record> {
    regular_file_len(path).filter(|&n| n <= 1024 * 1024)?;
    let record: Record = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (record.version == 1).then_some(record)
}

pub fn load(output: &Path, image: &Path) -> Option<Outcome> {
    let record = read_record(&output.join(RECORD))?;
    (record.image == image_stamps(image)?
        && log_stamp(image).is_none_or(|current| Some(current) == record.log_stamp))
    .then_some(record.outcome)
}

fn record_path(output: &Path) -> std::io::Result<std::path::PathBuf> {
    let path = output.join(RECORD);
    match fs::symlink_metadata(&path) {
        Ok(_) if read_record(&path).is_none() => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Existing dump status file is unrecognized",
        )),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(path),
    }
}

pub fn save(output: &Path, image: &Path, outcome: &Outcome) -> std::io::Result<()> {
    let destination = record_path(output)?;
    let log_stamp = log_stamp(image);
    let image =
        image_stamps(image).ok_or_else(|| std::io::Error::other("Dump image is incomplete"))?;
    let record = Record {
        version: 1,
        image,
        log_stamp,
        outcome: outcome.clone(),
    };
    let temp = output.join(format!("{RECORD}.{}.tmp", super::now_ms()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        serde_json::to_writer(&mut file, &record)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub fn invalidate(output: &Path) -> std::io::Result<()> {
    match fs::remove_file(record_path(output)?) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[derive(Clone, Debug, Default)]
pub struct SplitAssessment {
    checking: bool,
    current: Option<TrackErrors>,
    pending_blocked: bool,
    pub blocked: bool,
    pub checked: bool,
}

#[derive(Clone, Debug)]
struct TrackErrors {
    optional: bool,
    skip: u64,
    c2: u64,
    allowed_boundary_c2: u64,
    allowed_optional_c2: bool,
}

impl TrackErrors {
    fn blocks_split(&self) -> bool {
        if self.skip > 0 && !self.optional {
            return true;
        }
        if self.c2 == 0 {
            return false;
        }
        if self.optional {
            return !self.allowed_optional_c2;
        }
        self.c2 != self.allowed_boundary_c2
    }
}

fn count_after(line: &str, marker: &str) -> Option<u64> {
    line.split_once(marker)?
        .1
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

impl SplitAssessment {
    fn finish_track(&mut self) {
        if let Some(track) = self.current.take() {
            self.pending_blocked |= track.blocks_split();
        }
    }

    pub fn line(&mut self, line: &str) {
        let line = line.trim();
        if line == "checking tracks" {
            // Only the latest final check counts. Earlier read/refine diagnostics
            // must not mark a dump whose remaining errors have been recovered.
            self.checking = true;
            self.current = None;
            self.pending_blocked = false;
            self.blocked = false;
            self.checked = false;
            return;
        }
        if !self.checking {
            return;
        }
        if line == "done" {
            self.finish_track();
            self.blocked = self.pending_blocked;
            self.checked = true;
            self.checking = false;
            return;
        }
        if line.starts_with("*** ") {
            // An incomplete check is not a final split verdict.
            self.checking = false;
            self.current = None;
            return;
        }
        if let Some(tail) = line.strip_prefix("errors detected, track: ") {
            self.finish_track();
            if let Some((track, counts)) = tail.split_once(", sectors: ") {
                if let (Some(skip), Some(c2)) =
                    (count_after(counts, "SKIP:"), count_after(counts, "C2:"))
                {
                    self.current = Some(TrackErrors {
                        optional: track.parse::<u32>() == Ok(0)
                            || (!track.is_empty() && track.chars().all(|c| c == 'A')),
                        skip,
                        c2,
                        allowed_boundary_c2: 0,
                        allowed_optional_c2: false,
                    });
                    return;
                }
            }
            // An unknown diagnostic must never turn a failed track check into
            // evidence that it passed.
            self.pending_blocked = true;
        } else if let Some(track) = self.current.as_mut() {
            if line == "warning: CD-R lead-in/lead-out C2 errors detected" {
                track.allowed_optional_c2 = true;
            } else if line.starts_with("warning: CD-R leading C2 errors detected (sectors:")
                || line.starts_with("warning: CD-R trailing C2 errors detected (sectors:")
            {
                if let Some(count) = count_after(line, "(sectors:") {
                    track.allowed_boundary_c2 = track.allowed_boundary_c2.saturating_add(count);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn assess(lines: &[&str]) -> bool {
        let mut check = SplitAssessment::default();
        check.line("checking tracks");
        for line in lines {
            check.line(line);
        }
        check.line("done");
        check.blocked
    }

    #[test]
    fn main_track_skip_and_unresolved_c2_block_bin_generation() {
        assert!(assess(&[
            "errors detected, track: 01, sectors: {SKIP: 1, C2: 0}, samples: {SKIP: 588, C2: 0}"
        ]));
        assert!(assess(&[
            "errors detected, track: 02, sectors: {SKIP: 0, C2: 3}, samples: {SKIP: 0, C2: 24}"
        ]));
        assert!(
            assess(&[
                "errors detected, track: 01, sectors: {SKIP: 1, C2: 2}, samples: {SKIP: 1, C2: 2}",
                "warning: CD-R trailing C2 errors detected (sectors: 2)",
            ]),
            "boundary allowances cannot excuse unavailable main-track sectors"
        );
    }

    #[test]
    fn cdr_boundary_allowances_must_account_for_every_c2_sector() {
        let c2 =
            "errors detected, track: 01, sectors: {SKIP: 0, C2: 4}, samples: {SKIP: 0, C2: 999}";
        assert!(!assess(&[
            c2,
            "warning: CD-R trailing C2 errors detected (sectors: 4)"
        ]));
        assert!(!assess(&[
            c2,
            "warning: CD-R leading C2 errors detected (sectors: 1)",
            "warning: CD-R trailing C2 errors detected (sectors: 3)"
        ]));
        assert!(assess(&[
            c2,
            "warning: CD-R trailing C2 errors detected (sectors: 3)"
        ]));
        assert!(
            assess(&[
                c2,
                "warning: CD-R trailing C2 errors detected (sectors: 4)",
                "errors detected, track: 02, sectors: {SKIP: 0, C2: 1}, samples: {SKIP: 0, C2: 1}"
            ]),
            "allowances apply only to their own track"
        );
    }

    #[test]
    fn optional_tracks_follow_redumpers_cdr_exception() {
        for track in ["0", "00", "A", "AA"] {
            let skip = format!("errors detected, track: {track}, sectors: {{SKIP: 3, C2: 0}}, samples: {{SKIP: 3, C2: 0}}");
            assert!(!assess(&[&skip]));
            let c2 = format!("errors detected, track: {track}, sectors: {{SKIP: 3, C2: 1}}, samples: {{SKIP: 3, C2: 1}}");
            assert!(!assess(&[
                &c2,
                "warning: CD-R lead-in/lead-out C2 errors detected"
            ]));
            assert!(
                assess(&[&c2]),
                "non-CD-R C2 follows the splitter's rejection rule"
            );
        }
    }

    #[test]
    fn protection_warnings_and_recovered_read_errors_do_not_mark_a_clean_check() {
        let mut check = SplitAssessment::default();
        for line in [
            "*** DUMP",
            "LBA: 100/200, errors: { SCSI: 7, C2s: 3, Q: 2 }",
            "warning: protection detected",
            "*** REFINE",
            "LBA: 200/200, errors: { SCSI: 0, C2s: 0, Q: 0 }",
            "*** SPLIT",
            "warning: lead-out ends with unavailable sector (session: 1)",
            "checking tracks",
            "done",
        ] {
            check.line(line);
        }
        assert!(!check.blocked);
        // Recognized cheat-disc/ring/protection ranges do not occur in these
        // totals: redumper already excluded them before the track-check output.
        check.line("checking tracks");
        check.line(
            "errors detected, track: 01, sectors: {SKIP: 1, C2: 0}, samples: {SKIP: 1, C2: 0}",
        );
        check.line("done");
        assert!(check.blocked);
        check.line("checking tracks");
        check.line("done");
        assert!(!check.blocked);
        for _ in 0..1000 {
            check.line("hash/info output");
        }
        assert!(!check.blocked);
    }
}
