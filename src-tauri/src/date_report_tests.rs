use super::*;

// Change only timestamps in a real ISO fixture, then exercise both reporting
// routes: the flat-image dispatcher and the CUE data-track walker.
#[test]
fn latest_date_includes_directories_and_files_in_iso_and_cue() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso");
    let mut image = fs::read(fixture).unwrap();
    let root = 16 * 2048 + 156;
    image[root + 18..root + 25].copy_from_slice(&[110, 1, 1, 0, 0, 0, 0]);
    let mut pending = vec![(root, String::new())];
    let mut candidates = Vec::new();
    while let Some((record, parent)) = pending.pop() {
        let extent = u32::from_le_bytes(image[record + 2..record + 6].try_into().unwrap()) as usize * 2048;
        let size = u32::from_le_bytes(image[record + 10..record + 14].try_into().unwrap()) as usize;
        let mut pos = extent;
        while pos < extent + size {
            let len = image[pos] as usize;
            if len == 0 { pos = (pos / 2048 + 1) * 2048; continue; }
            let name_len = image[pos + 32] as usize;
            let name = image[pos + 33..pos + 33 + name_len].to_vec();
            image[pos + 18..pos + 25].copy_from_slice(&[110, 1, 1, 0, 0, 0, 0]);
            if name != [0] && name != [1] {
                let name = String::from_utf8(name).unwrap();
                let path = format!("{parent}/{}", name.split(';').next().unwrap());
                let is_dir = image[pos + 25] & 2 != 0;
                if is_dir { pending.push((pos, path.clone())); }
                candidates.push((pos, path, is_dir));
            }
            pos += len;
        }
    }
    let folder = candidates.iter().filter(|(_, _, dir)| *dir).max_by_key(|(_, path, _)| path.len()).unwrap();
    assert!(folder.1.matches('/').count() >= 2, "fixture must include a nested folder");
    let file = candidates.iter().find(|(_, _, dir)| !dir).unwrap();
    let temp = std::env::temp_dir().join(format!("dx-date-report-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir(&temp).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(temp.clone());
    let root_folder = (root, "/".to_owned(), true);
    for winner in [folder, file, &root_folder] {
        let mut bytes = image.clone();
        bytes[winner.0 + 18..winner.0 + 25].copy_from_slice(&[126, 10, 20, 12, 30, 0, 0]);
        fs::write(temp.join("dates.iso"), &bytes).unwrap();
        let mut raw = Vec::new();
        for sector in bytes.chunks_exact(2048) {
            let start = raw.len();
            raw.resize(start + 2352, 0);
            raw[start + 1..start + 11].fill(0xff);
            raw[start + 15] = 1;
            raw[start + 16..start + 2064].copy_from_slice(sector);
        }
        fs::write(temp.join("dates.bin"), raw).unwrap();
        fs::write(temp.join("dates.cue"), "FILE \"dates.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n").unwrap();
        for name in ["dates.iso", "dates.cue"] {
            let report = disc_date_report(temp.join(name).to_string_lossy().into_owned(), None).unwrap();
            assert_eq!(report.latest_date, "2026-10-20 12:30:00", "{name}: {}", winner.1);
            assert_eq!(report.latest_path, winner.1, "{name}");
            assert_eq!(report.entries_scanned as usize, candidates.len() + 1, "{name}");
        }
    }
}

#[test]
#[ignore = "requires Crash3-1998-10-09 via DX_IMG"]
fn crash3_root_directory_is_later_than_any_file() {
    let cue = std::env::var("DX_IMG").unwrap();
    let tracks = parse_cue_all_data_tracks(Path::new(&cue)).unwrap();
    for path in [&cue, &tracks[0].bin_path.to_string_lossy().into_owned()] {
        let report = disc_date_report(path.clone(), None).unwrap();
        assert_eq!(report.latest_path, "/", "{path}: {}", report.latest_date);
        assert_eq!(report.latest_date, "1998-10-11 13:22:11");
        assert_eq!(report.latest_file_date, "1998-10-09 11:39:20");
        assert_eq!(report.latest_file_path, "/ZDUMMY");
        assert_eq!(report.latest_directory_date, "1998-10-11 13:22:11");
        assert_eq!(report.latest_directory_path, "/");
        assert_eq!(report.absolute_latest_date, "1998-10-11 13:22:11");
        assert_eq!(report.absolute_latest_sources, ["Latest directory date", "PVD creation date"]);
        println!("{path}: {} {}", report.latest_path, report.latest_date);
    }
}

#[test]
fn absolute_latest_tracks_file_directory_and_both_pvd_dates_with_ties() {
    let mut report = DateReport::default();
    report.finish();
    assert!(report.absolute_latest_date.is_empty());
    assert!(report.absolute_latest_sources.is_empty());
    report.entry("2020-01-01 00:00:00", "/file".into(), false);
    report.entry("2019-01-01 00:00:00", "/folder".into(), true);
    report.finish();
    assert_eq!(report.absolute_latest_sources, ["Latest file date"]);
    report.entry("2021-01-01 00:00:00", "/".into(), true);
    report.pvd_created = "2021-01-01 00:00:00 UTC-4:45".into();
    report.finish();
    assert_eq!(report.absolute_latest_date, "2021-01-01 00:00:00");
    assert_eq!(report.absolute_latest_sources, ["Latest directory date", "PVD creation date"]);
    report.pvd_modified = "2022-01-01 00:00:00 UTC".into();
    report.finish();
    assert_eq!(report.absolute_latest_sources, ["PVD modification date"]);
    assert_eq!(report.latest_file_path, "/file");
    assert_eq!(report.latest_directory_path, "/");
}
