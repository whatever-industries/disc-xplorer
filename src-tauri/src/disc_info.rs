//! Read-only metadata for the Disc Info dialog. Uses the existing
//! readers; it does not change filesystem detection or scan directory contents.
use super::*;

#[derive(Serialize)]
pub struct InfoField {
    label: String,
    value: String,
}

#[derive(Serialize)]
pub struct InfoSection {
    title: String,
    fields: Vec<InfoField>,
}

#[derive(Serialize)]
pub struct DiscInfo {
    filesystems: Vec<String>,
    tracks: Vec<TrackInfo>,
    sections: Vec<InfoSection>,
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

fn primary_section(mut read: impl FnMut(u64) -> Option<Vec<u8>>, title: String) -> Option<InfoSection> {
    // Allow boot/supplementary descriptors before the primary, but bound reads
    // even for malformed images. Never interpret another format as ISO 9660.
    for lba in 16..80 {
        let bytes = read(lba)?;
        if bytes.len() < 2048 || &bytes[1..7] != b"CD001\x01" || bytes[0] == 255 { break; }
        if let Some(fields) = primary_fields(&bytes) {
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

fn collect_info(image_path: &str) -> Result<DiscInfo, String> {
    let path = Path::new(image_path);
    fs::metadata(path).map_err(|e| e.to_string())?;
    let filesystems = get_disc_filesystems(image_path.into())?;
    let tracks = track_list(path);
    let mut sections = Vec::new();
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
