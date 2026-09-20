//! DX_CASE_ROOT=<case-sensitive scratch directory> DX_MULTI_CUE=<mixed-mode cue>
//! cargo test --release cue_resolution_tests::integration_tests -- --ignored --nocapture

use super::*;

struct Fixture {
    scratch: Scratch,
    foreign: PathBuf,
    corrected: PathBuf,
    bins: Vec<PathBuf>,
    source: bincue::CueSheet,
}

fn fixture() -> Fixture {
    std::env::var_os("DX_CASE_ROOT").expect("set DX_CASE_ROOT to a case-sensitive volume");
    let source_path = PathBuf::from(std::env::var_os("DX_MULTI_CUE").expect("set DX_MULTI_CUE"));
    let source = bincue::parse(&source_path).unwrap();
    assert!(source.files.len() > 1);
    assert!(
        source.files.iter().all(|f| f.tracks.len() == 1),
        "use a one-BIN-per-track dump"
    );
    assert!(source.files.iter().any(|f| f.tracks[0].mode == "AUDIO"));
    assert!(source
        .files
        .iter()
        .any(|f| f.tracks[0].mode.starts_with("MODE")));
    let scratch = Scratch::new();
    let mut bins = Vec::new();
    for (i, file) in source.files.iter().enumerate() {
        let bin = scratch.0.join(format!("TrAcK-{}.BiN", i + 1));
        fs::copy(&file.path, &bin).unwrap();
        assert!(
            !scratch.0.join(format!("tRaCk-{}.bIn", i + 1)).exists(),
            "DX_CASE_ROOT must be case-sensitive"
        );
        bins.push(bin);
    }
    let mut foreign_text = String::new();
    let mut corrected_text = String::new();
    let mut index = 0;
    for line in fs::read_to_string(&source_path).unwrap().lines() {
        if line.trim().to_uppercase().starts_with("FILE ") {
            index += 1;
            foreign_text.push_str(&format!(
                "FILE \"Z:\\old-machine\\tRaCk-{index}.bIn\" BINARY\n"
            ));
            corrected_text.push_str(&format!("FILE \"TrAcK-{index}.BiN\" BINARY\n"));
        } else {
            foreign_text.push_str(line);
            foreign_text.push('\n');
            corrected_text.push_str(line);
            corrected_text.push('\n');
        }
    }
    assert_eq!(index, bins.len());
    let foreign = scratch.0.join("Foreign.cue");
    let corrected = scratch.0.join("Corrected.cue");
    fs::write(&foreign, foreign_text).unwrap();
    fs::write(&corrected, corrected_text).unwrap();
    Fixture {
        scratch,
        foreign,
        corrected,
        bins,
        source,
    }
}

fn digest(path: &Path) -> blake3::Hash {
    let mut file = File::open(path).unwrap();
    let mut hash = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    hash.finalize()
}

fn assert_same_data_track(actual: &DataTrack, expected: &DataTrack) {
    assert_eq!(
        (
            &actual.bin_path,
            actual.track_offset,
            actual.user_data_offset,
            actual.stride,
            actual.lba_offset,
            actual.descramble,
            actual.form2_edc_reserved,
            actual.sector_count
        ),
        (
            &expected.bin_path,
            expected.track_offset,
            expected.user_data_offset,
            expected.stride,
            expected.lba_offset,
            expected.descramble,
            expected.form2_edc_reserved,
            expected.sector_count
        ),
    );
}

#[test]
#[ignore]
fn foreign_multifile_cue_matches_corrected_tracks_sectors_and_round_trip() {
    let f = fixture();
    let foreign = f.foreign.to_str().unwrap();
    let corrected = f.corrected.to_str().unwrap();
    let actual_tracks = get_cue_tracks(foreign.into()).unwrap();
    let expected_tracks = get_cue_tracks(corrected.into()).unwrap();
    assert_eq!(
        serde_json::to_value(&actual_tracks).unwrap(),
        serde_json::to_value(&expected_tracks).unwrap()
    );
    assert_eq!(actual_tracks.len(), f.source.track_count());
    for (track, bin) in actual_tracks.iter().zip(&f.bins) {
        assert_eq!(Path::new(&track.bin_path), bin);
    }
    assert_same_data_track(
        &parse_cue_for_data_track(&f.foreign).unwrap(),
        &parse_cue_for_data_track(&f.corrected).unwrap(),
    );
    let actual_data = parse_cue_all_data_tracks(&f.foreign).unwrap();
    let expected_data = parse_cue_all_data_tracks(&f.corrected).unwrap();
    assert_eq!(actual_data.len(), expected_data.len());
    for (a, b) in actual_data.iter().zip(&expected_data) {
        assert_same_data_track(a, b);
    }

    // Sector View reads the first data track. Compare every raw sector with
    // independently located source bytes; also sample the corrected-CUE API.
    let first = &actual_data[0];
    let source_index = f.bins.iter().position(|p| p == &first.bin_path).unwrap();
    let mut original = File::open(&f.source.files[source_index].path).unwrap();
    original.seek(SeekFrom::Start(first.track_offset)).unwrap();
    let first_sector = read_sector_impl(foreign, 0).unwrap();
    assert!(first_sector.total_sectors > 16);
    let mut expected_bytes = vec![0u8; first.stride as usize];
    for lba in 0..first_sector.total_sectors {
        let sector = read_sector_impl(foreign, lba).unwrap();
        original.read_exact(&mut expected_bytes).unwrap();
        assert_eq!(sector.sector_size as u64, first.stride);
        assert!(
            sector.bytes == expected_bytes,
            "raw sector {lba} differs from source"
        );
        if [0, 16, first_sector.total_sectors - 1].contains(&lba) {
            assert_eq!(
                serde_json::to_value(&sector).unwrap(),
                serde_json::to_value(read_sector_impl(corrected, lba).unwrap()).unwrap()
            );
        }
    }
    let actual_fs = get_disc_filesystems(foreign.into()).unwrap();
    assert!(!actual_fs.is_empty());
    assert_eq!(actual_fs, get_disc_filesystems(corrected.into()).unwrap());

    let sheet = bincue::parse(&f.foreign).unwrap();
    assert_eq!(sheet.files.len(), f.source.files.len());
    assert_eq!(sheet.track_count(), f.source.track_count());
    assert_eq!(sheet.total_bytes(), f.source.total_bytes());
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let merged = f.scratch.0.join("Merged.cue");
    bincue::merge(&sheet, &merged, false, &cancel, |_, _| {}).unwrap();
    assert_eq!(
        fs::metadata(f.scratch.0.join("Merged.bin")).unwrap().len(),
        f.source.total_bytes()
    );
    let merged_sheet = bincue::parse(&merged).unwrap();
    assert_eq!(merged_sheet.files.len(), 1);
    assert_eq!(merged_sheet.track_count(), sheet.track_count());
    let out = f.scratch.0.join("split");
    fs::create_dir(&out).unwrap();
    let split_cue = out.join("Roundtrip.cue");
    bincue::split(&merged_sheet, &split_cue, false, &cancel, |_, _| {}).unwrap();
    let split_sheet = bincue::parse(&split_cue).unwrap();
    assert_eq!(split_sheet.track_count(), f.source.track_count());
    for file in &f.source.files {
        let track = &file.tracks[0];
        let split = out.join(bincue::track_filename(
            "Roundtrip",
            track.number,
            f.source.track_count(),
        ));
        assert_eq!(fs::metadata(&split).unwrap().len(), file.size);
        assert_eq!(
            digest(&split),
            digest(&file.path),
            "track {} differs after round trip",
            track.number
        );
        println!("TRACK|{}|{}|{}", track.number, file.size, digest(&split));
    }
    println!(
        "Matched {} tracks, {} data tracks, {} sectors and {} round-trip bytes",
        actual_tracks.len(),
        actual_data.len(),
        first_sector.total_sectors,
        f.source.total_bytes()
    );
}

#[test]
#[ignore]
fn actual_planners_refresh_after_unchanged_timestamp_mutations() {
    let f = fixture();
    let out = f.scratch.0.join("output");
    fs::create_dir(&out).unwrap();
    let expected_fs = get_disc_filesystems(f.corrected.to_string_lossy().into_owned()).unwrap();
    assert!(!expected_fs.is_empty());
    let stamp =
        filetime::FileTime::from_last_modification_time(&fs::metadata(&f.scratch.0).unwrap());
    let plan = |phase: &str, runnable: bool| {
        filetime::set_file_mtime(&f.scratch.0, stamp).unwrap();
        assert_eq!(
            filetime::FileTime::from_last_modification_time(&fs::metadata(&f.scratch.0).unwrap()),
            stamp
        );
        let sources = vec![f.foreign.to_string_lossy().into_owned()];
        let conversion = plan_batch_conversion(
            sources.clone(),
            out.to_string_lossy().into_owned(),
            None,
            false,
            "rename".into(),
            Some("merge".into()),
        )
        .unwrap();
        let extraction = plan_batch_extraction(
            sources,
            out.to_string_lossy().into_owned(),
            false,
            "rename".into(),
            "none".into(),
        )
        .unwrap();
        assert_eq!(conversion.items.len(), 1, "{phase}");
        assert_eq!(extraction.items.len(), 1, "{phase}");
        println!(
            "PLAN|{phase}|conversion={:?}|extraction={:?}|filesystems={:?}",
            conversion.items[0].problem,
            extraction.items[0].problem,
            extraction.items[0].filesystems
        );
        assert_eq!(
            conversion.items[0].problem.is_none(),
            runnable,
            "conversion: {phase}"
        );
        assert_eq!(
            extraction.items[0].problem.is_none(),
            runnable,
            "extraction: {phase}"
        );
        if runnable {
            assert_eq!(conversion.bytes_needed, f.source.total_bytes(), "{phase}");
            assert_eq!(extraction.bytes_needed, f.source.total_bytes(), "{phase}");
            assert_eq!(extraction.items[0].filesystems, expected_fs, "{phase}");
        } else {
            assert!(
                conversion.items[0]
                    .problem
                    .as_ref()
                    .unwrap()
                    .contains("Missing BIN"),
                "{phase}"
            );
            assert_eq!(conversion.bytes_needed, 0, "{phase}");
            assert_eq!(extraction.bytes_needed, 0, "{phase}");
            assert!(extraction.items[0].filesystems.is_empty(), "{phase}");
        }
    };
    let first = &f.bins[0];
    plan("present", true);
    let parked = f.scratch.0.join("parked.bin");
    fs::rename(first, &parked).unwrap();
    plan("renamed away", false);
    fs::rename(&parked, first).unwrap();
    plan("restored", true);
    let renamed = f.scratch.0.join("track-1.bin");
    fs::rename(first, &renamed).unwrap();
    plan("case-only rename", true);
    let collision = f.scratch.0.join("TRACK-1.BIN");
    fs::copy(&renamed, &collision).unwrap();
    plan("added collision", false);
    fs::remove_file(collision).unwrap();
    plan("removed collision", true);
    fs::remove_file(renamed).unwrap();
    plan("deleted", false);
    println!("Both planners matched expected results across seven unchanged-timestamp states");
}
