//! Interpret redumper's final CD split check, independently of read-time counters.
//! b760 cd/cd_split.ixx::check_tracks filters protection/skip ranges before logging
//! track totals. CD-R warnings then account for allowed boundary C2 sectors.
//! These are the same totals and exceptions used to block splitting without
//! --force-split. Do not treat all warnings or acquisition errors as this verdict.
#[derive(Clone, Debug, Default)]
pub struct SplitAssessment {
    checking: bool,
    current: Option<TrackErrors>,
    pending_blocked: bool,
    pub blocked: bool,
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
            return;
        }
        if !self.checking {
            return;
        }
        if line == "done" {
            self.finish_track();
            self.blocked = self.pending_blocked;
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
                }
            }
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
