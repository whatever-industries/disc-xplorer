use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("dx-extraction-{}-{time}-{n}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

#[test]
fn archives_reject_collisions_before_writing_any_files() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extraction");
    for kind in ["zip", "tar"] {
        for name in ["collision", "folders", "file-directory"] {
            let scratch = Scratch::new();
            let output = scratch.0.join("out");
            fs::create_dir(&output).unwrap();
            fs::write(output.join("sentinel"), b"keep me").unwrap();
            let input = fixtures.join(format!("{name}.{kind}"));
            let result = if kind == "zip" {
                zip_archive::ZipArchive::open(&input).unwrap().extract_directory("/", output.to_str().unwrap())
            } else {
                tar_archive::TarArchive::open(&input).unwrap().extract_directory("/", output.to_str().unwrap())
            };
            assert!(result.unwrap_err().contains("conflicting destination names"), "{name}.{kind}");
            assert_eq!(fs::read_dir(&output).unwrap().count(), 1, "nothing written for {name}.{kind}");
            assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep me");
        }
    }
}

#[test]
fn archives_still_extract_distinct_files_and_subdirectories() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extraction");
    for kind in ["zip", "tar"] {
        let scratch = Scratch::new();
        let output = scratch.0.join("out");
        let input = fixtures.join(format!("control.{kind}"));
        if kind == "zip" {
            zip_archive::ZipArchive::open(&input).unwrap().extract_directory("/", output.to_str().unwrap()).unwrap();
        } else {
            tar_archive::TarArchive::open(&input).unwrap().extract_directory("/", output.to_str().unwrap()).unwrap();
        }
        assert_eq!(fs::read(output.join("first.txt")).unwrap(), b"first source file");
        assert_eq!(fs::read(output.join("dir/second.txt")).unwrap(), b"second source file");
    }
}

// Exercise the same walker used by the disc filesystems, without depending on
// any parser's assumptions about which names a disc can contain.
struct Tree(Vec<(&'static str, bool)>);
impl ExtractFs for Tree {
    fn ls(&mut self, path: &str) -> Result<Vec<DiscEntry>, String> {
        Ok(self.0.iter().filter_map(|(full, dir)| {
            let (parent, name) = full.rsplit_once('/').unwrap();
            (parent == path.trim_end_matches('/')).then(|| DiscEntry {
                name: name.into(), is_dir: *dir, lba: 0, size: 1, size_bytes: 1,
                modified: String::new(), deleted: false, is_xa: false,
            })
        }).collect())
    }
    fn get(&mut self, path: &str, dest: &str) -> Result<(), String> {
        fs::write(dest, path).map_err(|e| e.to_string())
    }
}

#[test]
fn disc_walker_preflights_nested_collisions_before_writing() {
    for entries in [
        vec![("/track?.txt", false), ("/track_.txt", false)],
        vec![("/Readme", false), ("/README", false)],
        vec![("/a?", false), ("/a_", true)],
        vec![("/a?", true), ("/a_", true)],
        vec![("/first", false), ("/dir", true), ("/dir/a?", false), ("/dir/a_", false)],
    ] {
        let scratch = Scratch::new();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let error = extract_dir_tree(Tree(entries), &cancel, "/", scratch.0.to_str().unwrap()).unwrap_err();
        assert!(error.contains("conflicting destination names"));
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
    }
}

#[test]
fn disc_walker_keeps_separate_directories_and_checks_cancellation() {
    let scratch = Scratch::new();
    let tree = || Tree(vec![("/.", true), ("/..", true), ("/a", true), ("/a/file", false), ("/b", true), ("/b/file", false)]);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    extract_dir_tree(tree(), &cancel, "/", scratch.0.to_str().unwrap()).unwrap();
    assert_eq!(fs::read(scratch.0.join("a/file")).unwrap(), b"/a/file");
    assert_eq!(fs::read(scratch.0.join("b/file")).unwrap(), b"/b/file");
    let cancelled = scratch.0.join("cancelled");
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(extract_dir_tree(tree(), &cancel, "/", cancelled.to_str().unwrap()).unwrap_err(), "__cancelled__");
    assert!(!cancelled.join("a/file").exists());
}

// Compare whole-directory extraction against individually reading every source
// file. This exercises real names and nested directories, beyond a root listing.
fn verify_tree(reader: &mut dyn ExtractFs, output: &Path, reference: &Path) {
    let mut pending = vec![("/".to_string(), output.to_path_buf())];
    let mut files = 0;
    while let Some((source, dest)) = pending.pop() {
        for entry in reader.ls(&source).unwrap() {
            if matches!(entry.name.as_str(), "" | "." | "..") { continue; }
            let path = format!("{}/{}", source.trim_end_matches('/'), entry.name);
            let out = dest.join(sanitize_component(&entry.name));
            if entry.is_dir {
                assert!(out.is_dir());
                pending.push((path, out));
            } else {
                reader.get(&path, reference.to_str().unwrap()).unwrap();
                assert_eq!(fs::read(&out).unwrap(), fs::read(reference).unwrap(), "{path}");
                files += 1;
            }
        }
    }
    assert!(files > 0);
    println!("{files} extracted files match individual reads byte-for-byte");
}

/// DX_HFS_CUE=<real HFS cue> cargo test --release real_hfs_tree -- --ignored --nocapture
#[test]
#[ignore]
fn real_hfs_tree_preserves_every_file() {
    let image = std::env::var("DX_HFS_CUE").expect("set DX_HFS_CUE");
    let track = parse_cue_for_data_track(Path::new(&image)).unwrap();
    let scratch = Scratch::new();
    let out = scratch.0.join("out");
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    extract_dir_tree(open_hfs_fs(&track).unwrap(), &cancel, "/", out.to_str().unwrap()).unwrap();
    verify_tree(&mut open_hfs_fs(&track).unwrap(), &out, &scratch.0.join("reference"));
}

struct NitroReader(nitro_filesystem::NitroFs);
impl ExtractFs for NitroReader {
    fn ls(&mut self, path: &str) -> Result<Vec<DiscEntry>, String> { self.0.list_directory(path) }
    fn get(&mut self, path: &str, dest: &str) -> Result<(), String> { self.0.extract_file(path, dest) }
}

/// DX_NDS=<real ROM> cargo test --release real_nds_tree -- --ignored --nocapture
#[test]
#[ignore]
fn real_nds_tree_preserves_every_file() {
    let image = std::env::var("DX_NDS").expect("set DX_NDS");
    let scratch = Scratch::new();
    let out = scratch.0.join("out");
    let mut reader = nitro_filesystem::NitroFs::open(Path::new(&image)).unwrap();
    reader.extract_directory("/", out.to_str().unwrap()).unwrap();
    verify_tree(&mut NitroReader(reader), &out, &scratch.0.join("reference"));
}
