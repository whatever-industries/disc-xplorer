use super::*;

#[path = "cue_integration_tests.rs"]
mod integration_tests;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::var_os("DX_CASE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = root.join(format!("dx-cue-{}-{stamp}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cue_text(name: &str) -> String {
    format!("FILE \"{name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n")
}

#[test]
fn existing_relative_path_wins_over_same_named_sibling() {
    let dir = Scratch::new();
    fs::create_dir(dir.0.join("tracks")).unwrap();
    fs::write(dir.0.join("tracks/disc.bin"), b"right image").unwrap();
    fs::write(dir.0.join("disc.bin"), b"wrong image").unwrap();
    for name in ["tracks/disc.bin", r"tracks\disc.bin"] {
        assert_eq!(
            resolve_cue_file(&dir.0, name),
            dir.0.join("tracks/disc.bin")
        );
    }
}

#[test]
fn absolute_paths_and_foreign_basename_fallbacks() {
    let dir = Scratch::new();
    let external = Scratch::new();
    fs::write(external.0.join("disc.bin"), b"external").unwrap();
    fs::write(dir.0.join("disc.bin"), b"sibling").unwrap();
    let absolute = external.0.join("disc.bin");
    assert_eq!(
        resolve_cue_file(&dir.0, absolute.to_str().unwrap()),
        absolute
    );
    assert_eq!(
        resolve_cue_file(&dir.0, r"Z:\old-machine\disc.bin"),
        dir.0.join("disc.bin")
    );
}

#[test]
fn case_insensitive_fallback_finds_the_only_matching_file() {
    let dir = Scratch::new();
    let bin = dir.0.join("Disc.BIN");
    fs::write(&bin, b"data").unwrap();
    // Calling the fallback directly also covers it on case-insensitive hosts.
    assert_eq!(
        case_insensitive_sibling(&dir.0, "disc.bin"),
        Some(bin.clone())
    );
    let resolved = resolve_cue_file(&dir.0, r"Z:\old-machine\disc.bin");
    assert_eq!(
        fs::canonicalize(resolved).unwrap(),
        fs::canonicalize(bin).unwrap()
    );
}

#[test]
fn cache_refreshes_after_adding_renaming_and_removing_a_bin() {
    let dir = Scratch::new();
    let _cache = CueCacheScope::new();
    fs::write(dir.0.join("disc.cue"), cue_text("DISC.BIN")).unwrap();
    assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), None);
    // Advance the directory timestamp explicitly to cover filesystems whose
    // timestamps cannot distinguish several mutations in the same second.
    let initial = filetime::FileTime::from_last_modification_time(&fs::metadata(&dir.0).unwrap());
    let advance = |seconds| {
        filetime::set_file_mtime(
            &dir.0,
            filetime::FileTime::from_unix_time(initial.unix_seconds() + seconds, 0),
        )
        .unwrap();
    };
    let bin = dir.0.join("disc.bin");
    fs::write(&bin, b"data").unwrap();
    advance(2);
    assert_eq!(
        case_insensitive_sibling(&dir.0, "DISC.BIN"),
        Some(bin.clone())
    );
    let renamed = dir.0.join("renamed.bin");
    fs::rename(&bin, &renamed).unwrap();
    advance(4);
    assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), None);
    assert_eq!(
        case_insensitive_sibling(&dir.0, "RENAMED.BIN"),
        Some(renamed.clone())
    );
    fs::remove_file(renamed).unwrap();
    advance(6);
    assert_eq!(case_insensitive_sibling(&dir.0, "RENAMED.BIN"), None);
}

#[test]
fn unchanged_timestamps_do_not_hide_added_renamed_or_removed_bins() {
    let dir = Scratch::new();
    fs::write(dir.0.join("disc.cue"), cue_text("DISC.BIN")).unwrap();
    let stamp = filetime::FileTime::from_last_modification_time(&fs::metadata(&dir.0).unwrap());
    let restore = || {
        filetime::set_file_mtime(&dir.0, stamp).unwrap();
        assert_eq!(
            filetime::FileTime::from_last_modification_time(&fs::metadata(&dir.0).unwrap()),
            stamp
        );
    };
    // Invoke the fallback directly so case-insensitive hosts exercise the same
    // path as a mismatched filename on a case-sensitive filesystem.
    assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), None);
    let bin = dir.0.join("disc.bin");
    fs::write(&bin, b"image").unwrap();
    restore();
    assert_eq!(
        case_insensitive_sibling(&dir.0, "DISC.BIN"),
        Some(bin.clone())
    );
    let renamed = dir.0.join("renamed.bin");
    fs::rename(bin, &renamed).unwrap();
    restore();
    assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), None);
    assert_eq!(
        case_insensitive_sibling(&dir.0, "RENAMED.BIN"),
        Some(renamed.clone())
    );
    fs::remove_file(renamed).unwrap();
    restore();
    assert_eq!(case_insensitive_sibling(&dir.0, "RENAMED.BIN"), None);
}

#[test]
fn planning_snapshot_is_not_reused_by_other_threads_or_later_calls() {
    let dir = Scratch::new();
    fs::write(dir.0.join("disc.cue"), cue_text("DISC.BIN")).unwrap();
    let stamp = filetime::FileTime::from_last_modification_time(&fs::metadata(&dir.0).unwrap());
    let bin = dir.0.join("disc.bin");
    {
        let _cache = CueCacheScope::new();
        assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), None);
        fs::write(&bin, b"image").unwrap();
        filetime::set_file_mtime(&dir.0, stamp).unwrap();
        // One planning call reuses its directory snapshot, keeping thousands
        // of absent BIN lookups linear in the number of directory entries.
        assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), None);
        let other_dir = dir.0.clone();
        assert_eq!(
            std::thread::spawn(move || case_insensitive_sibling(&other_dir, "DISC.BIN"))
                .join()
                .unwrap(),
            Some(bin.clone())
        );
        {
            let _nested = CueCacheScope::new();
            assert_eq!(
                case_insensitive_sibling(&dir.0, "DISC.BIN"),
                Some(bin.clone())
            );
        }
        assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), None);
    }
    assert_eq!(
        case_insensitive_sibling(&dir.0, "DISC.BIN"),
        Some(bin.clone())
    );
    let _next_plan = CueCacheScope::new();
    assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), Some(bin));
}

#[test]
fn aborted_planning_does_not_leave_a_cached_miss() {
    let dir = Scratch::new();
    fs::write(dir.0.join("disc.cue"), cue_text("DISC.BIN")).unwrap();
    let stamp = filetime::FileTime::from_last_modification_time(&fs::metadata(&dir.0).unwrap());
    let bin = dir.0.join("disc.bin");
    let result = std::panic::catch_unwind(|| {
        let _cache = CueCacheScope::new();
        assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), None);
        fs::write(&bin, b"image").unwrap();
        filetime::set_file_mtime(&dir.0, stamp).unwrap();
        panic!("simulate aborted planning");
    });
    assert!(result.is_err());
    assert_eq!(case_insensitive_sibling(&dir.0, "DISC.BIN"), Some(bin));
}

#[test]
fn missing_bin_keeps_the_original_reference_in_the_error() {
    let dir = Scratch::new();
    let name = r"Z:\old-machine\missing.bin";
    let cue = dir.0.join("disc.cue");
    fs::write(&cue, cue_text(name)).unwrap();
    assert!(!resolve_cue_file(&dir.0, name).exists());
    let error = bincue::parse(&cue).err().expect("missing BIN must fail");
    assert_eq!(error, format!("Missing BIN file(s): {name}"));
}

/// DX_CASE_ROOT=<writable case-sensitive directory> cargo test --release
///   cue_resolution_tests::ambiguous -- --ignored --nocapture
#[test]
#[ignore]
fn ambiguous_case_matches_do_not_choose_an_arbitrary_image() {
    std::env::var_os("DX_CASE_ROOT").expect("set DX_CASE_ROOT to a case-sensitive directory");
    let dir = Scratch::new();
    fs::write(dir.0.join("Disc.bin"), b"first image").unwrap();
    assert!(
        !dir.0.join("DISC.BIN").exists(),
        "DX_CASE_ROOT must be case-sensitive"
    );
    fs::write(dir.0.join("DISC.BIN"), b"second image").unwrap();
    assert_eq!(resolve_cue_file(&dir.0, "Disc.bin"), dir.0.join("Disc.bin"));
    assert_eq!(resolve_cue_file(&dir.0, "DISC.BIN"), dir.0.join("DISC.BIN"));
    assert_eq!(case_insensitive_sibling(&dir.0, "disc.bin"), None);
    assert!(!resolve_cue_file(&dir.0, "disc.bin").exists());
}

// Exercise collision handling on every CI host, including case-insensitive ones.
#[test]
fn ambiguous_names_are_rejected_in_any_directory_order() {
    let paths = ["Disc.bin", "DISC.BIN", "disc.Bin", "other.bin"].map(PathBuf::from);
    for paths in [paths.to_vec(), paths.into_iter().rev().collect()] {
        let names = cue_sibling_index(paths);
        assert_eq!(names.get("disc.bin"), Some(&None));
        assert_eq!(
            names.get("other.bin"),
            Some(&Some(PathBuf::from("other.bin")))
        );
    }
}

/// DX_CDINTERLINK_CUE=<original cue> DX_CDINTERLINK_BIN=<known matching bin>
/// cargo test --release cue_resolution_tests::cdinterlink -- --ignored --nocapture
#[test]
#[ignore]
fn cdinterlink_browsing_and_extraction_match_a_corrected_cue() {
    let original = std::env::var("DX_CDINTERLINK_CUE").expect("set DX_CDINTERLINK_CUE");
    let bin = std::env::var("DX_CDINTERLINK_BIN")
        .expect("set DX_CDINTERLINK_BIN independently of the resolver");
    let scratch = Scratch::new();
    fs::copy(bin, scratch.0.join("reference.bin")).unwrap();
    let reference = scratch.0.join("reference.cue");
    let text = fs::read_to_string(&original).unwrap();
    assert_eq!(
        text.lines()
            .filter(|l| l.trim().starts_with("FILE "))
            .count(),
        1
    );
    let corrected: Vec<_> = text
        .lines()
        .map(|line| {
            if line.trim().starts_with("FILE ") {
                "FILE \"reference.bin\" BINARY"
            } else {
                line
            }
        })
        .collect();
    fs::write(&reference, corrected.join("\n")).unwrap();
    let reference = reference.to_str().unwrap().to_string();
    assert_eq!(
        get_disc_filesystems(original.clone()).unwrap(),
        vec!["CD-i"]
    );
    assert_eq!(
        get_disc_filesystems(reference.clone()).unwrap(),
        vec!["CD-i"]
    );
    assert_eq!(bincue::parse(Path::new(&original)).unwrap().files.len(), 1);

    let mut directories = vec!["/".to_string()];
    let mut visited = std::collections::HashSet::new();
    let mut files = 0;
    let mut bytes = 0;
    while let Some(path) = directories.pop() {
        assert!(visited.insert(path.clone()), "duplicate directory: {path}");
        assert!(
            visited.len() < 10_000,
            "directory traversal did not terminate"
        );
        let list = |image: &str| {
            let mut entries =
                list_disc_contents(image.to_string(), path.clone(), Some("CD-i".into()), false)
                    .unwrap();
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            entries
        };
        let actual = list(&original);
        let expected = list(&reference);
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "directory: {path}"
        );
        for entry in actual {
            let inner = format!("{}/{}", path.trim_end_matches('/'), entry.name);
            if entry.is_dir {
                directories.push(inner);
                continue;
            }
            let out = scratch.0.join(format!("actual-{files}"));
            let reference_out = scratch.0.join(format!("reference-{files}"));
            extract_single_file(
                original.clone(),
                inner.clone(),
                out.to_str().unwrap().into(),
                Some("CD-i".into()),
            )
            .unwrap();
            extract_single_file(
                reference.clone(),
                inner.clone(),
                reference_out.to_str().unwrap().into(),
                Some("CD-i".into()),
            )
            .unwrap();
            let actual_bytes = fs::read(out).unwrap();
            let expected_bytes = fs::read(reference_out).unwrap();
            assert!(
                actual_bytes == expected_bytes,
                "extracted bytes differ: {inner}"
            );
            assert_eq!(
                actual_bytes.len() as u64,
                entry.size_bytes as u64,
                "listed size differs: {inner}"
            );
            println!(
                "FILE|{inner}|{}|{}",
                actual_bytes.len(),
                blake3::hash(&actual_bytes)
            );
            files += 1;
            bytes += actual_bytes.len();
        }
    }
    assert!(files > 0, "no files extracted");
    println!(
        "Compared {} directories and {files} files, {bytes} extracted bytes",
        visited.len()
    );
}

// DX_CASE_ROOT=<case-sensitive directory> cargo test --release
//   unchanged_timestamp_tests -- --ignored --nocapture
mod unchanged_timestamp_tests {
    use super::*;

    fn scratch() -> Scratch {
        std::env::var_os("DX_CASE_ROOT").expect("set DX_CASE_ROOT");
        let dir = Scratch::new();
        fs::write(dir.0.join("disc.cue"), cue_text("dIsC.bIn")).unwrap();
        fs::write(dir.0.join("CaseProbe"), b"").unwrap();
        assert!(
            !dir.0.join("caseprobe").exists(),
            "requires a case-sensitive volume"
        );
        dir
    }

    fn stamp(dir: &Path) -> filetime::FileTime {
        filetime::FileTime::from_last_modification_time(&fs::metadata(dir).unwrap())
    }

    fn restore(dir: &Path, time: filetime::FileTime) {
        filetime::set_file_mtime(dir, time).unwrap();
        assert_eq!(
            stamp(dir),
            time,
            "directory timestamp must remain unchanged"
        );
    }

    #[test]
    #[ignore]
    fn added_bin_is_found_without_a_timestamp_change() {
        let dir = scratch();
        let time = stamp(&dir.0);
        assert!(!resolve_cue_file(&dir.0, "dIsC.bIn").exists());
        let bin = dir.0.join("Disc.bin");
        fs::write(&bin, b"new image").unwrap();
        restore(&dir.0, time);
        let actual = resolve_cue_file(&dir.0, "dIsC.bIn");
        println!(
            "ADD: expected={bin:?}, actual={actual:?}, resolved_exists={}",
            actual.exists()
        );
        assert_eq!(actual, bin, "a newly added BIN should be discoverable");
    }

    #[test]
    #[ignore]
    fn renamed_bin_is_found_without_a_timestamp_change() {
        let dir = scratch();
        let bin = dir.0.join("Disc.bin");
        fs::write(&bin, b"image").unwrap();
        let time = stamp(&dir.0);
        assert_eq!(resolve_cue_file(&dir.0, "dIsC.bIn"), bin);
        let renamed = dir.0.join("Renamed.bin");
        fs::rename(&bin, &renamed).unwrap();
        restore(&dir.0, time);
        let old = case_insensitive_sibling(&dir.0, "dIsC.bIn");
        let actual = resolve_cue_file(&dir.0, "rEnAmEd.bIn");
        println!("RENAME: old_cached={old:?}, expected={renamed:?}, actual={actual:?}, resolved_exists={}", actual.exists());
        assert!(
            old.is_none() && actual == renamed,
            "a rename should remove the old cached path and discover the new name"
        );
    }

    #[test]
    #[ignore]
    fn removed_bin_is_forgotten_without_a_timestamp_change() {
        let dir = scratch();
        let bin = dir.0.join("Disc.bin");
        fs::write(&bin, b"image").unwrap();
        let time = stamp(&dir.0);
        assert_eq!(resolve_cue_file(&dir.0, "dIsC.bIn"), bin);
        fs::remove_file(&bin).unwrap();
        restore(&dir.0, time);
        let actual = case_insensitive_sibling(&dir.0, "dIsC.bIn");
        println!(
            "REMOVE: cached={actual:?}, deleted_path_exists={}",
            bin.exists()
        );
        assert_eq!(
            actual, None,
            "a deleted path should not remain a cached match"
        );
    }

    #[test]
    #[ignore]
    fn added_collision_is_rejected_without_a_timestamp_change() {
        let dir = scratch();
        let bin = dir.0.join("Disc.bin");
        fs::write(&bin, b"first image").unwrap();
        let time = stamp(&dir.0);
        assert_eq!(resolve_cue_file(&dir.0, "dIsC.bIn"), bin);
        fs::write(dir.0.join("DISC.BIN"), b"second image").unwrap();
        restore(&dir.0, time);
        let actual = resolve_cue_file(&dir.0, "dIsC.bIn");
        println!(
            "COLLISION: resolved={actual:?}, resolved_exists={}",
            actual.exists()
        );
        assert!(
            !actual.exists(),
            "the cache must not bypass ambiguity rejection after a second matching file appears"
        );
    }
}
