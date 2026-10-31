//! Read-only metadata for the Disc Info dialog. Uses the existing
//! readers; it does not change filesystem detection or scan directory contents.
use super::*;

#[derive(Serialize)]
pub struct InfoField {
    pub(super) label: String,
    pub(super) value: String,
}

#[derive(Serialize)]
pub struct InfoSection {
    pub(super) title: String,
    pub(super) fields: Vec<InfoField>,
}

#[derive(Serialize)]
pub struct DiscInfo {
    pub(super) filesystems: Vec<String>,
    pub(super) tracks: Vec<TrackInfo>,
    pub(super) sections: Vec<InfoSection>,
}

fn field(fields: &mut Vec<InfoField>, label: &str, value: impl Into<String>) {
    let value = value.into();
    let value = value.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if !value.is_empty() {
        fields.push(InfoField { label: label.into(), value: value.into() });
    }
}

fn primary_fields(bytes: &[u8]) -> Option<Vec<InfoField>> {
    if bytes.len() < 2048 || &bytes[..7] != b"\x01CD001\x01" { return None; }
    let mut fields = Vec::new();
    // ISO 9660 primary descriptor fields; offsets match the vendored parser.
    for (label, start, end) in [
        ("System ID", 8, 40), ("Volume ID", 40, 72),
        ("Volume set ID", 190, 318), ("Publisher", 318, 446),
        ("Data preparer", 446, 574), ("Application", 574, 702),
        ("Copyright file", 702, 739),
    ] {
        field(&mut fields, label, String::from_utf8_lossy(&bytes[start..end]).into_owned());
    }
    field(&mut fields, "Created", decode_pvd_datetime(&bytes[813..830]));
    field(&mut fields, "Modified", decode_pvd_datetime(&bytes[830..847]));
    Some(fields)
}

// PlayStation system area, sector 4: a 32-byte heading followed by up to
// 38 bytes of license text. JP discs fill the remainder with ASCII zeroes.
// https://psx-spx.consoledev.net/dev/ps1/cdr/cdromformat/#system-area-prior-to-volume-descriptors
fn playstation_license(bytes: &[u8]) -> Option<String> {
    let heading = std::str::from_utf8(bytes.get(..32)?).ok()?;
    let heading = heading.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    if heading != "Licensed by" { return None; }
    let line = bytes.get(32..70)?;
    let end = line.iter().position(|b| matches!(b, 0 | b'\r' | b'\n')).unwrap_or(line.len());
    let line = &line[..end];
    if !line.iter().all(|b| b.is_ascii_graphic() || *b == b' ') { return None; }
    let publisher = std::str::from_utf8(line).ok()?.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    if !publisher.starts_with("Sony Computer Entertainment ") { return None; }
    // The US/EU names wrap within a word in the on-disc display layout.
    let publisher = publisher.replace("Amer ica", "America").replace("Euro pe", "Europe");
    Some(format!("{heading} {publisher}"))
}

fn primary_section(mut read: impl FnMut(u64) -> Option<Vec<u8>>, title: String) -> Option<InfoSection> {
    // Allow boot/supplementary descriptors before the primary, but bound reads
    // even for malformed images. Never interpret another format as ISO 9660.
    for lba in 16..80 {
        let bytes = read(lba)?;
        if bytes.len() < 2048 || &bytes[1..7] != b"CD001\x01" || bytes[0] == 255 { break; }
        if let Some(mut fields) = primary_fields(&bytes) {
            if let Some(license) = read(4).and_then(|bytes| playstation_license(&bytes)) {
                field(&mut fields, "License string (LBA 4)", license);
            }
            return Some(InfoSection { title, fields });
        }
    }
    None
}

#[tauri::command]
pub async fn disc_info(image_path: String) -> Result<DiscInfo, String> {
    tauri::async_runtime::spawn_blocking(move || collect_info(&image_path))
        .await.map_err(|e| e.to_string())?
}

pub(super) fn collect_info(image_path: &str) -> Result<DiscInfo, String> {
    let path = Path::new(image_path);
    fs::metadata(path).map_err(|e| e.to_string())?;
    let filesystems = get_disc_filesystems(image_path.into())?;
    let tracks = track_list(path);
    let mut sections = Vec::new();
    let volumes = lindbergh::discover(path);
    if !volumes.is_empty() {
        for volume in volumes {
            if let Some(mut section) = primary_section(|lba| volume.read(path, lba).map(|b| b.to_vec()), volume.label()) {
                field(&mut section.fields, "Start LBA", (volume.base / 2048).to_string());
                field(&mut section.fields, "Contents", "Stored payload files; decryption is not performed");
                sections.push(section);
            }
        }
        return Ok(DiscInfo { filesystems, tracks, sections });
    }
    for filesystem in &filesystems {
        if matches!(filesystem.as_str(), "Path Table" | "El Torito") { continue; }
        let mut fields = Vec::new();
        field(&mut fields, "Volume label", disc_volume_label(image_path.into(), Some(filesystem.clone())));
        if !fields.is_empty() { sections.push(InfoSection { title: filesystem.clone(), fields }); }
    }
    // Multi-session CUE/CDI images can carry a separate descriptor per data
    // track. Respect the parsed stride, payload offset and descrambling mode.
    let data_tracks = match ext_of(path).as_str() {
        "cue" => parse_cue_all_data_tracks(path).unwrap_or_default(),
        "cdi" => parse_cdi_all_data_tracks(path).unwrap_or_default().into_iter().map(|(t, _)| t).collect(),
        _ => Vec::new(),
    };
    if !data_tracks.is_empty() {
        for track in data_tracks {
            let Ok(mut file) = File::open(&track.bin_path) else { continue };
            let title = format!("ISO 9660 · {} · LBA {}", track.bin_path.file_name().unwrap_or_default().to_string_lossy(), track.track_offset / track.stride);
            if let Some(section) = primary_section(|lba| read_track_logical(&mut file, &track, lba).map(|b| b.to_vec()), title) {
                sections.push(section);
            }
        }
    } else if let Some(section) = primary_section(|lba| {
        let sector = read_sector_impl(image_path, lba).ok()?;
        let start = sector.user_data_offset as usize;
        sector.bytes.get(start..start + 2048).map(|b| b.to_vec())
    }, "ISO 9660".into()) { sections.push(section); }
    Ok(DiscInfo { filesystems, tracks, sections })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn license_sector(publisher: &[u8]) -> Vec<u8> {
        let mut bytes = vec![b'0'; 2048];
        bytes[..32].copy_from_slice(b"          Licensed  by          ");
        bytes[32..32 + publisher.len()].copy_from_slice(publisher);
        bytes
    }

    #[test]
    fn license_text_handles_region_padding_and_rejects_unrelated_data() {
        for (stored, expected) in [
            (b"Sony Computer Entertainment Amer  ica ".as_slice(), "America"),
            (b"Sony Computer Entertainment Euro pe   ".as_slice(), "Europe"),
            (b"Sony Computer Entertainment Inc.\n".as_slice(), "Inc."),
            (b"Sony Computer Entertainment of America\0".as_slice(), "of America"),
        ] {
            let mut bytes = license_sector(stored);
            if !stored.ends_with(b"\n") { bytes[32 + stored.len()..].fill(0); }
            assert_eq!(playstation_license(&bytes).unwrap(), format!("Licensed by Sony Computer Entertainment {expected}"));
        }
        assert!(playstation_license(&[]).is_none());
        assert!(playstation_license(&vec![0; 2048]).is_none());
        assert!(playstation_license(&license_sector(b"Unrelated text\0")).is_none());
        assert!(playstation_license(&license_sector(b"Sony Computer Entertainment \xff\0")).is_none());
    }

    #[test]
    fn license_field_uses_sector_four_payload_for_cooked_and_raw_tracks() {
        let temp = std::env::temp_dir().join(format!("dx-license-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&temp).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
        let _cleanup = Cleanup(temp.clone());
        let mut source = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso")).unwrap();
        source[4 * 2048..5 * 2048].copy_from_slice(&license_sector(b"Sony Computer Entertainment Inc.\n"));
        let iso_path = temp.join("license.iso");
        fs::write(&iso_path, &source).unwrap();
        let expected = "Licensed by Sony Computer Entertainment Inc.";
        let info = collect_info(iso_path.to_str().unwrap()).unwrap();
        assert!(info.sections.iter().flat_map(|s| &s.fields).any(|f| f.label == "License string (LBA 4)" && f.value == expected));
        for stride in [2352usize, 2448] {
            let path = temp.join(format!("license-{stride}.bin"));
            let mut raw = vec![0x55; stride * 2]; // stored pregap before INDEX 01
            for sector in source.chunks_exact(2048) {
                let start = raw.len();
                raw.resize(start + stride, 0);
                raw[start + 1..start + 11].fill(0xff);
                raw[start + 15] = 2;
                raw[start + 24..start + 2072].copy_from_slice(sector);
            }
            fs::write(&path, raw).unwrap();
            let track = DataTrack {
                bin_path: path.clone(), track_offset: (stride * 2) as u64, user_data_offset: 24,
                stride: stride as u64, lba_offset: 0, descramble: false,
                form2_edc_reserved: false, sector_count: (source.len() / 2048) as u64,
            };
            let mut file = File::open(&path).unwrap();
            let section = primary_section(|lba| read_track_logical(&mut file, &track, lba).map(|b| b.to_vec()), "ISO 9660".into()).unwrap();
            assert_eq!(section.fields.iter().find(|f| f.label == "License string (LBA 4)").unwrap().value, expected);
            if stride == 2352 {
                let cue = temp.join("license.cue");
                fs::write(&cue, "FILE \"license-2352.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 00 00:00:00\n    INDEX 01 00:00:02\n").unwrap();
                let info = collect_info(cue.to_str().unwrap()).unwrap();
                assert!(info.sections.iter().flat_map(|s| &s.fields).any(|f| f.label == "License string (LBA 4)" && f.value == expected));
            }
        }
    }

    #[test]
    #[ignore = "requires a local PlayStation image via DX_IMG"]
    fn real_playstation_license_matches_sector_view_bytes() {
        let path = std::env::var("DX_IMG").unwrap();
        let sector = read_sector_impl(&path, 4).unwrap();
        let start = sector.user_data_offset as usize;
        let license = playstation_license(&sector.bytes[start..]).unwrap();
        let info = collect_info(&path).unwrap();
        assert!(info.sections.iter().flat_map(|s| &s.fields).any(|f| f.label == "License string (LBA 4)" && f.value == license));
        println!("License: {license}");
    }

    #[test]
    fn headers_are_bounded_and_empty_fields_are_omitted() {
        assert!(primary_fields(&[]).is_none());
        let mut pvd = vec![0u8; 2048];
        assert!(primary_fields(&pvd).is_none());
        pvd[..7].copy_from_slice(b"\x01CD001\x01");
        pvd[40..44].copy_from_slice(b"GAME");
        pvd[318..327].copy_from_slice(b"Publisher");
        pvd[813..830].copy_from_slice(b"2026101412300000\0");
        let fields = primary_fields(&pvd).unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].value, "GAME");
        assert_eq!(fields[1].value, "Publisher");
        assert_eq!(fields[2].value, "2026-10-14 12:30:00 UTC");
        let mut boot = vec![0; 2048]; boot[..7].copy_from_slice(b"\0CD001\x01");
        let section = primary_section(|lba| Some(if lba == 16 { boot.clone() } else { pvd.clone() }), "ISO 9660".into()).unwrap();
        assert_eq!(section.fields[0].value, "GAME");
        let mut reads = 0;
        assert!(primary_section(|_| { reads += 1; Some(boot.clone()) }, "ISO 9660".into()).is_none());
        assert_eq!(reads, 64);
    }

    #[test]
    fn real_iso_metadata_matches_the_independent_filesystem_reader() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso");
        let info = collect_info(path.to_str().unwrap()).unwrap();
        let iso = ISO9660::new(File::open(&path).unwrap()).unwrap();
        let header = info.sections.iter().find(|s| s.fields.iter().any(|f| f.label == "Volume ID")).unwrap();
        let value = |label| header.fields.iter().find(|f| f.label == label).map(|f| f.value.as_str()).unwrap_or("");
        assert_eq!(value("Volume ID"), iso.volume_identifier().trim());
        assert_eq!(value("Publisher"), iso.publisher_identifier().trim());
        assert_eq!(value("Application"), iso.application_identifier().trim());
    }

    #[test]
    fn raw_header_read_respects_track_start_and_2448_byte_stride() {
        let path = std::env::temp_dir().join(format!("dx-info-stride-{}.bin", std::process::id()));
        let source = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso")).unwrap();
        let mut raw = vec![0x55; 2448];
        for sector in source.chunks_exact(2048) {
            let start = raw.len();
            raw.resize(start + 2448, 0x55);
            raw[start + 16..start + 16 + 2048].copy_from_slice(sector);
        }
        fs::write(&path, raw).unwrap();
        let track = DataTrack {
            bin_path: path.clone(), track_offset: 2448, user_data_offset: 16,
            stride: 2448, lba_offset: 0, descramble: false,
            form2_edc_reserved: false, sector_count: (source.len() / 2048) as u64,
        };
        let actual = {
            let mut file = File::open(&path).unwrap();
            primary_section(|lba| read_track_logical(&mut file, &track, lba).map(|b| b.to_vec()), "ISO 9660".into())
        };
        fs::remove_file(path).unwrap();
        let actual = actual.unwrap();
        let expected = primary_fields(&source[16 * 2048..17 * 2048]).unwrap();
        assert_eq!(serde_json::to_value(actual.fields).unwrap(), serde_json::to_value(expected).unwrap());
    }
}
