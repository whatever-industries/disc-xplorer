//! Conservative acceptance of intentional CD protection ranges.
//!
//! Some protected CDs deliberately contain a large non-CD sector range. Redumper
//! records that range, still validates the written tracks, and reports zero
//! REDUMP.INFO errors. Those signals are more authoritative than the transient
//! read counters the UI receives while dumping.
use std::fs;
use std::path::Path;

pub const GOOD_MESSAGE: &str = "Dump is good. Intentional C2 errors are expected.";

fn regular(path: &Path) -> Option<fs::Metadata> {
    fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
}

fn nonzero_invalid_sync(line: &str) -> bool {
    line.trim()
        .strip_prefix("invalid sync sectors:")
        .and_then(|tail| tail.trim().split_whitespace().next())
        .and_then(|count| count.parse::<u64>().ok())
        .is_some_and(|count| count > 0)
}

/// Return true only for a completed Redumper run whose protection-induced
/// invalid sectors were accepted by Redumper's final track and REDUMP.INFO
/// checks. This deliberately does not treat `REDUMP.INFO errors: 0` alone as a
/// general success verdict: an actual intentional-range signal is required.
pub fn verified_errors(output: &Path, name: &str) -> bool {
    let log_path = output.join(format!("{name}.log"));
    let Some(metadata) = regular(&log_path) else {
        return false;
    };
    if metadata.len() == 0 || metadata.len() > 16 * 1024 * 1024 {
        return false;
    }
    let Ok(log) = fs::read_to_string(log_path) else {
        return false;
    };
    // Redumper appends each retry/refinement. Only the latest complete run may
    // establish a success verdict.
    let Some(latest) = log.rsplit("\n=== ").next() else {
        return false;
    };
    if !latest.contains("redumper (build:")
        || !latest
            .trim_end()
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("*** END "))
    {
        return false;
    }
    let lines: Vec<_> = latest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let clean_track_check = lines
        .windows(2)
        .any(|pair| pair == ["checking tracks", "done"]);
    let redump_info_clean = lines.contains(&"REDUMP.INFO errors: 0");
    let intentional_range = lines.iter().copied().any(nonzero_invalid_sync)
        && lines
            .iter()
            .any(|line| line.starts_with("warning: descramble failed (LBA: "));
    clean_track_check
        && redump_info_clean
        && intentional_range
        && !lines
            .iter()
            .any(|line| line.starts_with("errors detected, track:") || line.starts_with("error:"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture(log: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "dx-intentional-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("Disc.log"), log).unwrap();
        path
    }

    #[test]
    fn accepts_redump_clean_intentional_protection_range() {
        let path = fixture("=== dump\nredumper (build: b760)\nchecking tracks\ndone\nwarning: descramble failed (LBA: [30 .. 244049])\ninvalid sync sectors: 244020 (LBA: 30-244049)\nREDUMP.INFO errors: 0\n*** END (time check: 1s)\n");
        assert!(verified_errors(&path, "Disc"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn rejects_unresolved_track_error_or_plain_zero_count() {
        let unresolved = fixture("=== dump\nredumper (build: b760)\nchecking tracks\ndone\nwarning: descramble failed (LBA: [30 .. 244049])\ninvalid sync sectors: 244020 (LBA: 30-244049)\nerrors detected, track: 1\nREDUMP.INFO errors: 0\n*** END (time check: 1s)\n");
        assert!(!verified_errors(&unresolved, "Disc"));
        let _ = std::fs::remove_dir_all(unresolved);
        let ordinary = fixture("=== dump\nredumper (build: b760)\nchecking tracks\ndone\nREDUMP.INFO errors: 0\n*** END (time check: 1s)\n");
        assert!(!verified_errors(&ordinary, "Disc"));
        let _ = std::fs::remove_dir_all(ordinary);
    }
}
