//! Conservative exception for a completed single-track CD-R whose only damaged
//! sectors are the one or two run-out sectors beyond its ISO or mapped HFS volume. This is a
//! dump verdict, not a change to filesystem detection or image extraction.
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub const GOOD_MESSAGE: &str = "Dump is good. CD-R end sector errors are expected.";

fn regular(path: &Path) -> Option<fs::Metadata> {
    fs::symlink_metadata(path).ok().filter(|m| m.is_file())
}

// Some CD-Rs disagree by one sector about the transition into lead-out.
// Accept only that transition, never an arbitrary TOC discrepancy.
fn end_toc_mismatch(line: &str, total: u64) -> bool {
    [
        "warning: TOC / QTOC mismatch, track length (track: 1, LBA: ",
        "warning: TOC / QTOC mismatch, track index 00 (track: A, LBA: ",
        "warning: TOC / QTOC mismatch, track index 01 (track: A, LBA: ",
    ].iter().any(|prefix| {
        let Some(tail) = line.strip_prefix(prefix).and_then(|s| s.strip_suffix(')')) else { return false; };
        let Some((a, b)) = tail.split_once(" <=> ") else { return false; };
        matches!((a.parse::<u64>(), b.parse::<u64>()), (Ok(a), Ok(b)) if a == total && a.abs_diff(b) == 1)
    })
}

pub fn verified_end_errors(output: &Path, name: &str) -> bool {
    verify(output, name).unwrap_or(false)
}

// This verifier accepts only the MODE1/2352 layout established by the CUE.
// Keep these volume bounds local to the dump verdict, not filesystem detection.
fn read_block(bin: &mut (impl Read + Seek), block: u64) -> Option<[u8; 512]> {
    bin.seek(SeekFrom::Start(block / 4 * 2352 + 16 + block % 4 * 512)).ok()?;
    let mut bytes = [0; 512];
    bin.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

fn mapped_hfs_end(bin: &mut (impl Read + Seek)) -> Option<u64> {
    let be16 = |b: &[u8]| u16::from_be_bytes(b.try_into().unwrap()) as u64;
    let be32 = |b: &[u8]| u32::from_be_bytes(b.try_into().unwrap()) as u64;
    let driver = read_block(bin, 0)?;
    if &driver[..2] != b"ER" || be16(&driver[2..4]) != 512 { return None; }
    let blocks = be32(&driver[4..8]);
    let first = read_block(bin, 1)?;
    let count = be32(&first[4..8]);
    if !(2..=128).contains(&count) || count >= blocks { return None; }
    let mut has_hfs = false;
    for index in 1..=count {
        let entry = read_block(bin, index)?;
        if &entry[..2] != b"PM" || be32(&entry[4..8]) != count { return None; }
        let start = be32(&entry[8..12]);
        let length = be32(&entry[12..16]);
        if length == 0 || start + length > blocks { return None; }
        let kind = entry[48..80].split(|&b| b == 0).next()?;
        match kind {
            b"Apple_partition_map" if index == 1 && start == 1 && length >= count => {},
            b"Apple_Free" => {},
            b"Apple_HFS" if index > 1 && start > count && length >= 5 => {
                let mdb = read_block(bin, start + 2)?;
                let alloc_count = be16(&mdb[18..20]);
                let alloc_size = be32(&mdb[20..24]);
                let alloc_start = be16(&mdb[28..30]);
                if &mdb[..2] != b"BD" || alloc_count == 0 || alloc_size == 0
                    || alloc_size % 512 != 0 || alloc_start < 3
                    || alloc_start + alloc_count * (alloc_size / 512) > length - 2
                { return None; }
                has_hfs = true;
            },
            _ => return None,
        }
    }
    has_hfs.then_some(blocks.div_ceil(4))
}

fn verify(output: &Path, name: &str) -> Option<bool> {
    let file = |ext: &str| output.join(format!("{name}.{ext}"));
    // Bound sidecar reads, and do not follow links outside the dump folder.
    for (ext, max) in [("log", 16 * 1024 * 1024), ("cue", 4096), ("atip", 4096)] {
        let len = regular(&file(ext))?.len();
        if len == 0 || len > max {
            return Some(false);
        }
    }
    let atip = fs::read(file("atip")).ok()?;
    if atip.len() < 28 || usize::from(u16::from_be_bytes([atip[0], atip[1]])) + 2 != atip.len() {
        return Some(false);
    }
    let cue = fs::read_to_string(file("cue")).ok()?;
    let lines: Vec<_> = cue
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if lines
        != [
            format!("FILE \"{name}.bin\" BINARY"),
            "TRACK 01 MODE1/2352".into(),
            "INDEX 01 00:00:00".into(),
        ]
    {
        return Some(false);
    }
    let log = fs::read_to_string(file("log")).ok()?;
    // Redumper appends refinements. An earlier successful run cannot bless a
    // later interrupted run or a new error.
    let latest = log.rsplit("\n=== ").next()?;
    if !latest.contains("redumper (build:") {
        return Some(false);
    }
    if !latest.trim_end().lines().last()?.starts_with("*** END ") {
        return Some(false);
    }
    let bin_meta = regular(&file("bin"))?;
    let state_meta = regular(&file("state"))?;
    if bin_meta.len() % 2352 != 0 {
        return Some(false);
    }
    let total = bin_meta.len() / 2352;
    let mut offset = None;
    let mut track_errors = 0;
    for line in latest.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("disc write offset: ") {
            offset = Some(value.parse::<i64>().ok()?);
        }
        // A single logged offset cannot describe variable-offset state data.
        if line == "offset shift correction applied:" {
            return Some(false);
        }
        if line.starts_with("errors detected, track:") {
            if !line.starts_with("errors detected, track: 1, sectors: ") {
                return Some(false);
            }
            track_errors += 1;
        }
        if line.starts_with("error:") {
            return Some(false);
        }
        if line.starts_with("warning:") && !end_toc_mismatch(line, total) && ![
            "warning: TOC / QTOC mismatch, nonexistent track in QTOC (track: A)",
            "warning: non-zero data range is not continuous, skipping Universal Hash calculation",
            "warning: incomplete pre-gap (session: 1, unavailable:",
            "warning: lead-in contains non-zero data (session: 1, sectors:",
            "warning: lead-in starts with unavailable sector (session: 1)",
            "warning: lead-out ends with unavailable sector (session: 1)",
            "warning: CD-R trailing C2 errors detected (sectors:",
        ].iter().any(|allowed| line.starts_with(allowed)) { return Some(false); }
    }
    // Some CD-Rs have invalid run-out modes but no failed read samples, so
    // redumper's track check succeeds without an "errors detected" line.
    let log_lines: Vec<_> = latest.lines().map(str::trim).filter(|s| !s.is_empty()).collect();
    let clean_track_check = log_lines.windows(2).any(|pair| pair == ["checking tracks", "done"])
        && log_lines.contains(&"REDUMP.INFO errors: 0");
    if track_errors > 1 || (track_errors == 0 && !clean_track_check) {
        return Some(false);
    }
    let offset = offset?;
    if !(-588 * 4..=588 * 4).contains(&offset) {
        return Some(false);
    }
    let mut bin = BufReader::new(File::open(file("bin")).ok()?);
    let mut sector = [0u8; 2352];
    bin.seek(SeekFrom::Start(16 * 2352)).ok()?;
    bin.read_exact(&mut sector).ok()?;
    let pvd = &sector[16..2064];
    let blocks = if &pvd[..7] == b"\x01CD001\x01" {
        let blocks = u32::from_le_bytes(pvd[80..84].try_into().ok()?) as u64;
        if pvd[128..132] != [0, 8, 8, 0]
            || blocks != u32::from_be_bytes(pvd[84..88].try_into().ok()?) as u64
        { return Some(false); }
        blocks
    } else {
        mapped_hfs_end(&mut bin)?
    };
    if blocks <= 16 || total <= blocks {
        return Some(false);
    }
    // Check all sectors preceding the last two, including any padding after
    // the volume. The filesystem need not end immediately at the run-out.
    let clean_end = blocks.max(total - 2);
    // Only SUCCESS samples are accepted. Disabled C2/SCSI checking, missing
    // samples, and C2 flags before the run-out prevent this exception.
    let mut state = BufReader::new(File::open(file("state")).ok()?);
    state
        // redumper read_entry subtracts its byte_offset argument, and the
        // splitter passes -write_offset: the resulting position adds it.
        .seek(SeekFrom::Start((45150 * 588 + offset) as u64))
        .ok()?;
    bin.seek(SeekFrom::Start(0)).ok()?;
    let mut samples = [0u8; 588];
    for _ in 0..clean_end {
        bin.read_exact(&mut sector).ok()?;
        state.read_exact(&mut samples).ok()?;
        if sector[..12] != crate::CD_SYNC
            || sector[15] != 1
            || !crate::sector_edc_ok(&sector, false)
            || samples.iter().any(|&s| s != 4)
        {
            return Some(false);
        }
    }
    // Require actual boundary damage, rather than using stale log counters.
    let mut bad_end = false;
    for _ in clean_end..total {
        bin.read_exact(&mut sector).ok()?;
        bad_end |= sector[..12] != crate::CD_SYNC
            || sector[15] != 1
            || !crate::sector_edc_ok(&sector, false);
    }
    // Do not accept files modified while the check was running.
    for (ext, before) in [("bin", bin_meta), ("state", state_meta)] {
        let after = regular(&file(ext))?;
        if before.len() != after.len() || before.modified().ok()? != after.modified().ok()? {
            return Some(false);
        }
    }
    Some(bad_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "dx-cdr-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&dir).unwrap();
            let f = Self(dir);
            f.write(
                "cue",
                b"FILE \"Disc.bin\" BINARY\n TRACK 01 MODE1/2352\n INDEX 01 00:00:00\n",
            );
            let mut atip = [0; 28];
            atip[1] = 26;
            f.write("atip", &atip);
            f.write("log", b"=== test ===\nredumper (build: b760)\ndisc write offset: +3\nchecking tracks\nerrors detected, track: 1, sectors: {SKIP: 1, C2: 1}, samples: {SKIP: 9, C2: 432}\ndone\n*** END (time check: 2s)\n");
            let mut data = vec![0; 34 * 2352];
            for n in 0..32 {
                let sector = &mut data[n * 2352..(n + 1) * 2352];
                sector[..12].copy_from_slice(&crate::CD_SYNC);
                sector[15] = 1;
                if n == 16 {
                    sector[16..23].copy_from_slice(b"\x01CD001\x01");
                    sector[96..100].copy_from_slice(&32u32.to_le_bytes());
                    sector[100..104].copy_from_slice(&32u32.to_be_bytes());
                    sector[144..148].copy_from_slice(&[0, 8, 8, 0]);
                }
                let edc = crate::edc_compute(&sector[..2064]);
                sector[2064..2068].copy_from_slice(&edc.to_le_bytes());
            }
            f.write("bin", &data);
            let mut state = File::create(f.0.join("Disc.state")).unwrap();
            state.seek(SeekFrom::Start(45150 * 588 + 3)).unwrap();
            state.write_all(&vec![4; 32 * 588]).unwrap();
            f
        }
        fn write(&self, ext: &str, bytes: &[u8]) {
            fs::write(self.0.join(format!("Disc.{ext}")), bytes).unwrap();
        }
        fn good(&self) -> bool {
            verified_end_errors(&self.0, "Disc")
        }
    }

    #[test]
    fn only_verified_cdr_runout_errors_are_excused() {
        let f = Fixture::new();
        assert!(f.good());
        let original = fs::read(f.0.join("Disc.bin")).unwrap();
        let mut corrupt = original.clone();
        corrupt[20 * 2352 + 100] ^= 1;
        f.write("bin", &corrupt);
        assert!(!f.good(), "filesystem checksum damage");
        f.write("bin", &original);
        let mut state = fs::OpenOptions::new()
            .write(true)
            .open(f.0.join("Disc.state"))
            .unwrap();
        state.seek(SeekFrom::Start((45150 + 20) * 588 + 3)).unwrap();
        state.write_all(&[1]).unwrap();
        assert!(!f.good(), "C2 inside filesystem even with valid EDC");
        state.seek(SeekFrom::Start((45150 + 20) * 588 + 3)).unwrap();
        state.write_all(&[4]).unwrap();
        assert!(f.good());
        fs::remove_file(f.0.join("Disc.atip")).unwrap();
        assert!(!f.good(), "not proven recordable media");
    }

    #[test]
    fn incomplete_runs_other_warnings_and_other_layouts_keep_review() {
        let f = Fixture::new();
        let log = fs::read_to_string(f.0.join("Disc.log")).unwrap();
        f.write(
            "log",
            format!("{log}\n=== new run ===\nredumper (build: b760)\n*** REFINE\n").as_bytes(),
        );
        assert!(!f.good());
        f.write(
            "log",
            log.replace(
                "checking tracks",
                "warning: unexpected read issue\nchecking tracks",
            )
            .as_bytes(),
        );
        assert!(!f.good());
        f.write("log", log.as_bytes());
        let mut bin = fs::read(f.0.join("Disc.bin")).unwrap();
        bin.extend_from_slice(&[0; 2352]);
        f.write("bin", &bin);
        assert!(!f.good(), "more than two trailing sectors");
        bin.truncate(34 * 2352);
        f.write("bin", &bin);
        f.write("cue", b"FILE \"Disc.bin\" BINARY\n TRACK 01 MODE1/2352\n INDEX 01 00:00:00\n TRACK 02 AUDIO\n INDEX 01 00:00:32\n");
        assert!(
            !f.good(),
            "mixed-mode images are not covered by the exception"
        );
    }

    #[test]
    fn clean_padding_and_one_sector_leadout_disagreement_are_allowed() {
        let f = Fixture::new();
        let mut bin = fs::read(f.0.join("Disc.bin")).unwrap();
        let padding = bin[..2352].repeat(150);
        bin.splice(32 * 2352..32 * 2352, padding);
        f.write("bin", &bin);
        let mut state = fs::OpenOptions::new()
            .write(true)
            .open(f.0.join("Disc.state"))
            .unwrap();
        state.seek(SeekFrom::Start((45150 + 32) * 588 + 3)).unwrap();
        state.write_all(&vec![4; 150 * 588]).unwrap();
        let log = fs::read_to_string(f.0.join("Disc.log")).unwrap();
        let warnings = concat!(
            "warning: TOC / QTOC mismatch, track length (track: 1, LBA: 184 <=> 185)\n",
            "warning: TOC / QTOC mismatch, track index 00 (track: A, LBA: 184 <=> 185)\n",
            "warning: TOC / QTOC mismatch, track index 01 (track: A, LBA: 184 <=> 185)\n",
        );
        f.write(
            "log",
            log.replace("checking tracks", &format!("{warnings}checking tracks"))
                .as_bytes(),
        );
        assert!(f.good());
        // Even padding must be clean; a small ISO volume must not hide errors.
        bin[50 * 2352 + 100] ^= 1;
        f.write("bin", &bin);
        assert!(!f.good());
    }

    #[test]
    fn toc_disagreements_must_be_at_the_end_and_only_one_sector() {
        for line in [
            "warning: TOC / QTOC mismatch, track length (track: 1, LBA: 34 <=> 36)",
            "warning: TOC / QTOC mismatch, track length (track: 1, LBA: 20 <=> 21)",
            "warning: TOC / QTOC mismatch, track index 01 (track: 1, LBA: 34 <=> 35)",
            "warning: TOC / QTOC mismatch, track index 01 (track: A, LBA: unknown)",
        ] {
            assert!(!end_toc_mismatch(line, 34));
        }
    }

    #[test]
    fn mapped_hfs_runout_with_clean_track_check_is_expected() {
        let f = Fixture::new();
        f.write("log", b"=== test ===\nredumper (build: b761)\ndisc write offset: +3\nmedia errors:\n  Q: 145\nwarning: TOC / QTOC mismatch, nonexistent track in QTOC (track: A)\nchecking tracks\ndone\nREDUMP.INFO errors: 0\n*** END (time check: 0s)\n");
        let mut bin = fs::read(f.0.join("Disc.bin")).unwrap();
        bin[16 * 2352 + 16..16 * 2352 + 2064].fill(0);
        let block = |data: &mut [u8], n: usize, offset: usize, bytes: &[u8]| {
            let pos = n / 4 * 2352 + 16 + n % 4 * 512 + offset;
            data[pos..pos + bytes.len()].copy_from_slice(bytes);
        };
        block(&mut bin, 0, 0, b"ER\x02\x00\x00\x00\x00\x80");
        for (n, start, length, kind) in [(1, 1u32, 2u32, "Apple_partition_map"), (2, 4, 124, "Apple_HFS")] {
            block(&mut bin, n, 0, b"PM\0\0\0\0\0\x02");
            block(&mut bin, n, 8, &start.to_be_bytes());
            block(&mut bin, n, 12, &length.to_be_bytes());
            block(&mut bin, n, 48, kind.as_bytes());
        }
        block(&mut bin, 6, 0, b"BD");
        block(&mut bin, 6, 18, &100u16.to_be_bytes());
        block(&mut bin, 6, 20, &512u32.to_be_bytes());
        block(&mut bin, 6, 28, &3u16.to_be_bytes());
        let write = |data: &mut [u8]| {
            for sector in data[..32 * 2352].chunks_exact_mut(2352) {
                let edc = crate::edc_compute(&sector[..2064]);
                sector[2064..2068].copy_from_slice(&edc.to_le_bytes());
            }
            f.write("bin", data);
        };
        write(&mut bin);
        assert!(f.good(), "HFS CD-R with clean reads and invalid run-out modes");
        let mut corrupt = bin.clone();
        corrupt[20 * 2352 + 100] ^= 1;
        f.write("bin", &corrupt);
        assert!(!f.good(), "REDUMP.INFO zero cannot hide interior damage");
        block(&mut bin, 0, 4, &136u32.to_be_bytes());
        write(&mut bin);
        assert!(!f.good(), "volume includes damaged sectors");
        block(&mut bin, 0, 4, &128u32.to_be_bytes());
        block(&mut bin, 2, 12, &125u32.to_be_bytes());
        write(&mut bin);
        assert!(!f.good(), "partition extends past declared volume");
        block(&mut bin, 2, 12, &124u32.to_be_bytes());
        block(&mut bin, 6, 18, &124u16.to_be_bytes());
        write(&mut bin);
        assert!(!f.good(), "allocation area extends past partition");
    }

    #[test]
    fn clean_track_check_requires_zero_summary_and_completed_check() {
        let f = Fixture::new();
        let log = "=== test ===\nredumper (build: b761)\ndisc write offset: +3\nchecking tracks\ndone\nREDUMP.INFO errors: 0\n*** END (time check: 0s)\n";
        f.write("log", log.as_bytes());
        assert!(f.good(), "also covers ISO CD-Rs without read failures");
        f.write("log", log.replace("errors: 0", "errors: 1").as_bytes());
        assert!(!f.good());
        f.write("log", log.replace("checking tracks\ndone", "checking tracks").as_bytes());
        assert!(!f.good());
    }

    #[test]
    #[ignore]
    fn real_cdr_end_errors() {
        let path = std::env::var("DX_DUMP_FOLDER").unwrap();
        let output = Path::new(&path);
        assert!(verified_end_errors(
            output,
            output.file_name().unwrap().to_str().unwrap()
        ));
    }
}
