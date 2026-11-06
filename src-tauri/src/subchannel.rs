//! Read-only inspection of matching CD subchannel sidecars. No patches are
//! applied to the image, and missing SBI bytes are never reconstructed.
//! Formats: psx-spx CDROM Subchannel Images and Kippykip/SBITools' LSD spec.
use super::*;
use std::collections::BTreeMap;

const MAX_PATCH_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Serialize)]
pub struct Record {
    /// Unknown bytes (SBI, especially types 2/3) remain null.
    q: Vec<Option<u8>>,
    channels: Option<Vec<Vec<u8>>>,
    adr: Option<u8>,
    control: Option<u8>,
    track: Option<String>,
    index: Option<String>,
    relative_msf: Option<String>,
    absolute_msf: Option<String>,
    recorded_crc: Option<u16>,
    calculated_crc: Option<u16>,
    crc_valid: Option<bool>,
    timing_matches: Option<bool>,
}

#[derive(Serialize)]
pub struct Source {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) format: String,
    pub(super) records: u64,
    pub(super) first_lba: Option<i64>,
    pub(super) last_lba: Option<i64>,
    pub(super) issue: Option<String>,
    record: Option<Record>,
    previous_sector: Option<u64>,
    next_sector: Option<u64>,
    previous_lba: Option<i64>,
    next_lba: Option<i64>,
}

#[derive(Default, Serialize)]
pub struct Report {
    disc_lba: Option<i64>,
    record_only: bool,
    pub(super) sources: Vec<Source>,
}

#[derive(Clone)]
enum Stored {
    Sub(u64), // CloneCD: P..W, eight non-interleaved 12-byte channels.
    Patch(Arc<BTreeMap<i64, Vec<Option<u8>>>>),
}

fn bcd(value: u8) -> Option<i64> {
    ((value >> 4) <= 9 && (value & 15) <= 9)
        .then_some(i64::from(value >> 4) * 10 + i64::from(value & 15))
}

fn msf_lba(bytes: &[u8]) -> Option<i64> {
    let m = bcd(*bytes.first()?)?;
    let s = bcd(*bytes.get(1)?)?;
    let f = bcd(*bytes.get(2)?)?;
    (s < 60 && f < 75).then_some((m * 60 + s) * 75 + f - 150)
}

fn header_lba(bytes: &[u8]) -> Option<i64> {
    const SYNC: &[u8] = b"\0\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\0";
    if bytes.get(..12)? != SYNC { return None; }
    msf_lba(bytes.get(12..15)?)
}

// Q uses complemented CRC-16/CCITT, initial value 0 (MMC READ CD sub-Q).
fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 { crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 }; }
    }
    !crc
}

fn decode(q: Vec<Option<u8>>, channels: Option<Vec<Vec<u8>>>, expected: Option<i64>) -> Record {
    let adr = q[0].map(|b| b & 15);
    let control = q[0].map(|b| b >> 4);
    let position = |start: usize| -> Option<Vec<u8>> {
        if adr.is_some_and(|value| value != 1) { return None; }
        q[start..start + 3].iter().copied().collect()
    };
    let msf_text = |bytes: &[u8]| format!("{:02X}:{:02X}:{:02X}", bytes[0], bytes[1], bytes[2]);
    let relative_msf = position(3).map(|b| msf_text(&b));
    let absolute = position(7);
    let absolute_msf = absolute.as_ref().map(|b| msf_text(b));
    // None means not position Q / not enough data; false includes malformed BCD.
    let timing_matches = absolute.as_ref().zip(expected).map(|(b, expected)| msf_lba(b) == Some(expected));
    let calculated_crc = q[..10].iter().copied().collect::<Option<Vec<_>>>().map(|b| crc16(&b));
    let recorded_crc = q[10].zip(q[11]).map(|(a,b)| u16::from_be_bytes([a,b]));
    let crc_valid = recorded_crc.zip(calculated_crc).map(|(a,b)| a == b);
    let number = |i: usize| if adr == Some(1) { q[i].map(|b| bcd(b).map_or_else(|| format!("0x{b:02X}"), |n| n.to_string())) } else { None };
    Record { adr, control, track: number(1), index: number(2), relative_msf, absolute_msf,
        recorded_crc, calculated_crc, crc_valid, timing_matches, q, channels }
}

fn parse_patch(bytes: &[u8], sbi: bool) -> Result<Stored, String> {
    let mut pos = if sbi {
        if !bytes.starts_with(b"SBI\0") { return Err("Invalid SBI header.".into()); }
        4
    } else { 0 };
    let mut records = BTreeMap::new();
    while pos < bytes.len() {
        let address = bytes.get(pos..pos + 3).and_then(msf_lba).ok_or("Invalid or truncated sidecar address.")?;
        pos += 3;
        let (start, count) = if sbi {
            let kind = *bytes.get(pos).ok_or("Truncated SBI record.")?;
            pos += 1;
            match kind { 1 => (0,10), 2 => (3,3), 3 => (7,3), _ => return Err(format!("Unsupported SBI record type: {kind}.")) }
        } else { (0,12) };
        let data = bytes.get(pos..pos + count).ok_or("Truncated sidecar record.")?;
        pos += count;
        let mut q = vec![None; 12];
        for (slot, byte) in q[start..start + count].iter_mut().zip(data) { *slot = Some(*byte); }
        // Multiple partial SBI records for one address may complement each
        // other, but conflicting data must not silently overwrite evidence.
        if records.len() >= 65536 { return Err("Too many subchannel patch records.".into()); }
        let entry = records.entry(address).or_insert_with(|| vec![None; 12]);
        for (slot, value) in entry.iter_mut().zip(q) {
            if slot.is_some() && value.is_some() && *slot != value { return Err("Conflicting sidecar records.".into()); }
            if value.is_some() { *slot = value; }
        }
    }
    Ok(Stored::Patch(Arc::new(records)))
}

// Sparse patches are small; cache their validated contents using the file's
// identity/mtime/size. SUB payloads are never loaded in full or scanned.
type CacheEntry = (PathBuf, u64, Option<std::time::SystemTime>, Result<Stored, String>);
fn stored(path: &Path) -> Result<Stored, String> {
    static CACHE: OnceLock<Mutex<Vec<CacheEntry>>> = OnceLock::new();
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    let key = (meta.len(), meta.modified().ok());
    let mut cache = CACHE.get_or_init(Default::default).lock().map_err(|e| e.to_string())?;
    if let Some(entry) = cache.iter().find(|e| e.0 == path && (e.1,e.2) == key) { return entry.3.clone(); }
    let result = if ext_of(path) == "sub" {
        if meta.len() == 0 || meta.len() % 96 != 0 { Err("SUB size must be a nonzero multiple of 96 bytes.".into()) }
        else { Ok(Stored::Sub(meta.len() / 96)) }
    } else if meta.len() > MAX_PATCH_BYTES {
        Err("Subchannel patch exceeds the 4 MiB inspection limit.".into())
    } else {
        // Limit the read too, in case a file grows after metadata was checked.
        let mut bytes = Vec::new();
        File::open(path).and_then(|f| f.take(MAX_PATCH_BYTES + 1).read_to_end(&mut bytes)).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_PATCH_BYTES { return Err("Subchannel patch exceeds the 4 MiB inspection limit.".into()); }
        parse_patch(&bytes, ext_of(path) == "sbi")
    };
    cache.retain(|e| e.0 != path);
    if cache.len() >= 8 { let _ = cache.remove(0); }
    cache.push((path.into(), key.0, key.1, result.clone()));
    result
}

// Exact basename only. Never attach an arbitrary sidecar from the folder or
// search recursively. Mixed-case extensions work on case-sensitive volumes.
fn siblings(path: &Path) -> Vec<PathBuf> {
    let Some(stem) = path.file_stem() else { return Vec::new() };
    let stem = stem.to_string_lossy();
    let mut found = Vec::new();
    for ext in ["sub", "lsd", "sbi"] {
        let exact = path.with_extension(ext);
        if exact.is_file() { found.push(exact); continue; }
        let matches: Vec<_> = fs::read_dir(path.parent().unwrap_or(Path::new("."))).into_iter().flatten()
            .filter_map(Result::ok).map(|e| e.path())
            .filter(|p| ext_of(p) == ext && p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(&stem)) && p.is_file())
            .take(2).collect();
        if matches.len() == 1 { found.push(matches[0].clone()); }
    }
    found
}

pub(super) fn inspect(image: &Path, sector: u64) -> Result<Report, String> {
    inspect_record(image, sector, None)
}

fn inspect_record(image: &Path, sector: u64, record_lba: Option<i64>) -> Result<Report, String> {
    let ext = ext_of(image);
    // Match the primary track used by Sector View, including stored pregaps.
    let track = match ext.as_str() {
        "cue" => parse_cue_all_data_tracks(image).ok().and_then(|mut t| if t.is_empty() { None } else { Some(t.remove(0)) }),
        "ccd" => parse_ccd_for_data_track(image).ok(),
        _ => None,
    };
    let mut paths = siblings(image);
    if let Some(track) = &track { for p in siblings(&track.bin_path) { if !paths.contains(&p) { paths.push(p); } } }
    if paths.is_empty() { return Ok(Report::default()); }
    let image_text = image.to_string_lossy();
    let first = read_sector_impl(&image_text, 0)?;
    let total = first.total_sectors;
    if sector >= total { return Err("Subchannel sector is outside the image.".into()); }
    // Header at the track's start fixes the coordinate mapping; a deliberately
    // wrong Q timestamp at the current sector never changes this mapping.
    let disc_base = header_lba(&first.bytes);
    let disc_lba = record_lba.or_else(|| disc_base.and_then(|base| base.checked_add(i64::try_from(sector).ok()?)));
    let to_sector = |lba: i64| lba.checked_sub(disc_base?).and_then(|n| u64::try_from(n).ok()).filter(|n| *n < total);
    let record_only = record_lba.is_some_and(|lba| to_sector(lba).is_none());
    let backing = track.as_ref().map_or(image, |t| t.bin_path.as_path());
    let file_base = track.as_ref().map_or(0, |t| t.track_offset / t.stride);
    let mut sources = Vec::new();
    for path in paths {
        let mut source = Source { id: path.to_string_lossy().into(), name: path.file_name().unwrap_or_default().to_string_lossy().into(),
            format: match ext_of(&path).as_str() { "sub" => "SUB · P–W (CloneCD)", "lsd" => "LSD · Q", _ => "SBI · Q patch" }.into(),
            records: 0, first_lba: None, last_lba: None, issue: None, record: None,
            previous_sector: None, next_sector: None, previous_lba: None, next_lba: None };
        match stored(&path) {
            Err(e) => source.issue = Some(e),
            Ok(Stored::Patch(records)) => {
                source.records = records.len() as u64;
                source.first_lba = records.keys().next().copied();
                source.last_lba = records.keys().next_back().copied();
                if let Some(lba) = disc_lba {
                    source.record = records.get(&lba).map(|q| decode(q.clone(), None, Some(lba)));
                    source.previous_lba = records.range(..lba).next_back().map(|(key,_)| *key);
                    source.next_lba = records.range((std::ops::Bound::Excluded(lba), std::ops::Bound::Unbounded)).next().map(|(key,_)| *key);
                    source.previous_sector = source.previous_lba.and_then(to_sector);
                    source.next_sector = source.next_lba.and_then(to_sector);
                } else {
                    source.issue = Some("Disc LBA unavailable; sidecar records cannot be aligned.".into());
                    // Still allow inspecting a patch independently of image
                    // geometry (e.g. a cooked ISO has no raw CD MSF header).
                    source.next_lba = records.keys().next().copied();
                }
            }
            Ok(Stored::Sub(count)) => {
                source.records = count;
                // A matching backing-file SUB aligns one-to-one with that file.
                // A disc-level CUE sidecar instead begins at disc LBA zero.
                let flat_backing = track.is_some() || matches!(ext.as_str(), "bin" | "img" | "iso");
                let same_backing = flat_backing && path.parent() == backing.parent() && path.file_stem().zip(backing.file_stem())
                    .is_some_and(|(a,b)| a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()));
                let origin = if same_backing { disc_base.and_then(|b| b.checked_sub(i64::try_from(file_base).ok()?)) } else { Some(0) };
                let index = if record_lba.is_some() {
                    disc_lba.zip(origin).and_then(|(lba, origin)| lba.checked_sub(origin)).and_then(|n| u64::try_from(n).ok())
                } else if same_backing { file_base.checked_add(sector) } else { disc_lba.and_then(|n| u64::try_from(n).ok()) };
                source.first_lba = origin;
                source.last_lba = origin.and_then(|b| b.checked_add(i64::try_from(count - 1).ok()?));
                if let Some(index) = index.filter(|n| *n < count) {
                    let mut bytes = [0u8; 96];
                    let result = File::open(&path).and_then(|mut f| { f.seek(SeekFrom::Start(index * 96))?; f.read_exact(&mut bytes) });
                    match result {
                        Err(e) => source.issue = Some(e.to_string()),
                        Ok(()) => {
                            let q = bytes[12..24].iter().copied().map(Some).collect();
                            source.record = Some(decode(q, Some(bytes.chunks_exact(12).map(|c| c.to_vec()).collect()), disc_lba));
                        }
                    }
                } else { source.issue = Some("No corresponding subchannel sector.".into()); }
                if same_backing {
                    let expected = fs::metadata(backing).ok().map(|m| m.len() / first.sector_size as u64);
                    if expected != Some(count) { source.issue = Some("SUB sector count differs from the image.".into()); }
                }
            }
        }
        sources.push(source);
    }
    Ok(Report { disc_lba, record_only, sources })
}

#[tauri::command]
pub async fn read_subchannels(image_path: String, lba: u64, record_lba: Option<i64>) -> Result<Report, String> {
    tauri::async_runtime::spawn_blocking(move || inspect_record(Path::new(&image_path), lba, record_lba)).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!("dx-subchannels-{}-{}-{}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn raw(&self, name: &str, first_lba: i64, count: usize) -> PathBuf {
            let mut bytes = vec![0; count * 2352];
            for (i, frame) in bytes.chunks_exact_mut(2352).enumerate() {
                let address = first_lba + i as i64 + 150;
                let bcd = |n: i64| ((n / 10) * 16 + n % 10) as u8;
                frame[1..11].fill(0xff);
                frame[12] = bcd(address / 4500);
                frame[13] = bcd(address / 75 % 60);
                frame[14] = bcd(address % 75);
                frame[15] = 2;
            }
            let p = self.0.join(name);
            fs::write(&p, bytes).unwrap(); p
        }
    }
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    // Published psx-spx CloneCD sample, not generated by our CRC routine.
    const Q: [u8;12] = [0x41,1,1,0,0,0,0,0,2,0,0x28,0x32];

    #[test]
    fn q_checksum_position_and_missing_sbi_bytes_are_reported_honestly() {
        assert_eq!(crc16(&Q[..10]), 0x2832);
        let full = decode(Q.map(Some).to_vec(), None, Some(0));
        assert_eq!(full.crc_valid, Some(true));
        assert_eq!(full.timing_matches, Some(true));
        assert_eq!(full.track.as_deref(), Some("1"));
        let mut wrong = Q; wrong[9] = 0x80;
        let bad = decode(wrong.map(Some).to_vec(), None, Some(0));
        assert_eq!(bad.crc_valid, Some(false));
        assert_eq!(bad.timing_matches, Some(false));
        let mut sbi = b"SBI\0\x00\x02\x00\x01".to_vec(); sbi.extend_from_slice(&Q[..10]);
        let Stored::Patch(patch) = parse_patch(&sbi, true).unwrap() else { panic!() };
        let record = decode(patch[&0].clone(), None, Some(0));
        assert_eq!(&record.q[10..], &[None,None]);
        assert_eq!(record.recorded_crc, None);
        assert_eq!(record.calculated_crc, Some(0x2832));
        assert_eq!(record.crc_valid, None);
        // SBI type 2/3 store just one timestamp, not a fabricated full Q.
        let Stored::Patch(patch) = parse_patch(b"SBI\0\x00\x02\x00\x02\x00\x00\x01\x00\x02\x00\x03\x00\x02\x02", true).unwrap() else { panic!() };
        let record = decode(patch[&0].clone(), None, Some(0));
        assert_eq!(record.q.iter().filter(|v| v.is_some()).count(), 6);
        assert_eq!(record.relative_msf.as_deref(), Some("00:00:01"));
        assert_eq!(record.calculated_crc, None);
        assert_eq!(record.timing_matches, Some(false));
        // A catalog/ISRC Q record must not be interpreted as track/time Q.
        wrong[0] = 0x42;
        let record = decode(wrong.map(Some).to_vec(), None, Some(0));
        assert_eq!(record.track, None);
        assert_eq!(record.absolute_msf, None);
    }

    #[test]
    fn patch_readers_reject_truncation_bad_addresses_and_conflicts() {
        // SBITools' published LSD example: deliberately unusual Q preserved.
        let lsd = [3,8,5, 0x41,1,1,7,6,5,0,0x23,8,5,0x38,0x39];
        let Stored::Patch(patch) = parse_patch(&lsd, false).unwrap() else { panic!() };
        assert_eq!(patch[&13955][10..], [Some(0x38),Some(0x39)]);
        for n in 1..lsd.len() { assert!(parse_patch(&lsd[..n], false).is_err()); }
        assert!(parse_patch(b"SUB\0", true).is_err());
        assert!(parse_patch(b"SBI\0\x00\x02\x00\x04", true).is_err());
        assert!(parse_patch(b"SBI\0\x00\x60\x00\x02\0\0\0", true).is_err());
        assert!(parse_patch(b"SBI\0\x00\x02\x00\x02\0\0\0\x00\x02\x00\x02\0\0\x01", true).is_err());
        // PC/Mac protection records can precede LBA 0.
        assert_eq!(msf_lba(&[0,1,0x74]), Some(-1));
    }

    #[test]
    fn clonecd_sub_reads_all_channels_and_uses_backing_file_offset() {
        let f = Fixture::new();
        let img = f.raw("Disc.img", -2, 5);
        let ccd = f.0.join("Disc.ccd");
        fs::write(&ccd,"[CloneCD]\nVersion=3\n[Entry 0]\nPoint=1\nControl=4\nPLBA=2\n").unwrap();
        let mut sub = vec![0x55; 5 * 96];
        sub[2*96+12..2*96+24].copy_from_slice(&Q);
        fs::write(img.with_extension("SUB"), &sub).unwrap();
        let info = inspect(&ccd, 0).unwrap();
        assert_eq!(info.disc_lba, Some(0));
        let source = &info.sources[0];
        assert_eq!(source.first_lba, Some(-2));
        assert_eq!(source.last_lba, Some(2));
        let record = source.record.as_ref().unwrap();
        assert_eq!(record.q, Q.map(Some));
        assert_eq!(record.channels.as_ref().unwrap()[7], vec![0x55;12]);
        assert_eq!(record.crc_valid, Some(true));
        assert!(source.issue.is_none());
        // Count mismatch remains visible, rather than hiding the sidecar.
        fs::write(img.with_extension("SUB"), &sub[..4*96]).unwrap();
        assert!(inspect(&ccd,0).unwrap().sources[0].issue.is_some());
    }

    #[test]
    fn cue_disc_and_track_sidecars_have_distinct_origins_and_cache_refreshes() {
        let f = Fixture::new();
        f.raw("Track 2.bin", 8, 6);
        let cue = f.0.join("Disc.cue");
        fs::write(&cue,"FILE \"Track 2.bin\" BINARY\n TRACK 02 MODE2/2352\n INDEX 01 00:00:02\n").unwrap();
        let mut disc_sub = vec![0; 20*96]; disc_sub[10*96+12..10*96+24].copy_from_slice(&Q);
        fs::write(f.0.join("Disc.sub"), disc_sub).unwrap();
        let mut track_sub = vec![0; 6*96]; track_sub[2*96+12..2*96+24].copy_from_slice(&Q);
        fs::write(f.0.join("Track 2.sub"),track_sub).unwrap();
        let mut patch = b"SBI\0\x00\x02\x10\x01".to_vec(); patch.extend_from_slice(&Q[..10]);
        fs::write(f.0.join("Disc.sbi"),&patch).unwrap();
        fs::write(f.0.join("Unrelated.sbi"),&patch).unwrap();
        let info = inspect(&cue,0).unwrap();
        assert_eq!(info.disc_lba,Some(10));
        assert_eq!(info.sources.len(),3);
        for s in &info.sources { assert_eq!(s.record.as_ref().unwrap().q[..10],Q[..10].iter().copied().map(Some).collect::<Vec<_>>()); }
        assert_eq!(info.sources[0].first_lba, Some(0));
        assert_eq!(info.sources[2].first_lba, Some(8));
        // A newly added address is visible on the next read, not cached forever.
        patch.extend_from_slice(b"\x00\x02\x12\x01"); patch.extend_from_slice(&Q[..10]);
        fs::write(f.0.join("Disc.sbi"), &patch).unwrap();
        let info = inspect(&cue,0).unwrap();
        assert_eq!(info.sources[1].records,2);
        assert_eq!(info.sources[1].next_sector,Some(2));
        assert!(inspect(&cue,4).is_err());
    }

    #[test]
    fn malformed_sidecar_never_blocks_main_image_and_missing_alignment_is_explicit() {
        let f = Fixture::new();
        let iso = f.0.join("Disc.iso"); fs::write(&iso,vec![0;4096]).unwrap();
        fs::write(iso.with_extension("sub"),[0;95]).unwrap();
        fs::write(iso.with_extension("sbi"),b"SBI\0\x00\x02\x00\x02\0\0\0").unwrap();
        let report = inspect(&iso,0).unwrap();
        assert!(report.sources.iter().all(|s| s.issue.is_some()));
        assert!(report.sources.iter().all(|s| s.record.is_none()));
        assert!(read_sector_impl(iso.to_str().unwrap(),0).is_ok());
        fs::remove_file(iso.with_extension("sub")).unwrap();
        fs::remove_file(iso.with_extension("sbi")).unwrap();
        assert!(inspect(&iso,0).unwrap().sources.is_empty());
    }

    #[test]
    fn disc_info_includes_sparse_sidecars_without_claiming_missing_bytes_were_recorded() {
        let f = Fixture::new();
        let iso = f.0.join("Disc.iso");
        fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso"), &iso).unwrap();
        let mut patch = b"SBI\0\x03\x08\x05\x01".to_vec(); patch.extend_from_slice(&Q[..10]);
        fs::write(iso.with_extension("sbi"),patch).unwrap();
        let info = disc_info::collect_info(iso.to_str().unwrap()).unwrap();
        let section = info.sections.iter().find(|s| s.title == "Disc.sbi").unwrap();
        assert!(section.fields.iter().any(|f| f.label == "Records" && f.value == "1"));
        assert!(section.fields.iter().any(|f| f.label == "First disc LBA" && f.value == "13955"));
        assert!(section.fields.iter().any(|f| f.label == "Inspection" && f.value.contains("Subchannels")));
        assert!(section.fields.iter().any(|f| f.label == "Status" && f.value.contains("cannot be aligned")));
    }

    #[test]
    fn sparse_records_outside_the_image_remain_readable_without_inventing_image_sectors() {
        let f = Fixture::new();
        let image = f.raw("Disc.bin", 0, 2);
        let mut patch = b"SBI\0".to_vec();
        for msf in [[0,1,0x74],[0,2,0],[0,2,3]] {
            patch.extend_from_slice(&msf); patch.push(1); patch.extend_from_slice(&Q[..10]);
        }
        fs::write(image.with_extension("sbi"), &patch).unwrap();
        let info = inspect(&image,0).unwrap();
        assert_eq!(info.sources[0].previous_lba,Some(-1));
        assert_eq!(info.sources[0].previous_sector,None);
        for address in [-1,3] {
            let info = inspect_record(&image,0,Some(address)).unwrap();
            assert_eq!(info.disc_lba,Some(address));
            assert!(info.record_only);
            assert_eq!(info.sources[0].record.as_ref().unwrap().q[..10],Q[..10].iter().copied().map(Some).collect::<Vec<_>>());
        }
        let info = inspect_record(&image,0,Some(-1)).unwrap();
        assert_eq!(info.sources[0].next_lba,Some(0));
        assert_eq!(info.sources[0].next_sector,Some(0));
        let info = inspect_record(&image,0,Some(0)).unwrap();
        assert!(!info.record_only);
        // Cooked images have no raw MSF header, but their matching sidecars
        // must still be readable explicitly as standalone records.
        let iso = image.with_extension("iso"); fs::write(&iso,vec![0;4096]).unwrap();
        let info = inspect(&iso,0).unwrap();
        assert_eq!(info.sources[0].next_lba,Some(-1));
        let info = inspect_record(&iso,0,Some(-1)).unwrap();
        assert!(info.record_only);
        assert!(info.sources[0].record.is_some());
    }

    #[test]
    fn same_named_sources_in_different_directories_keep_distinct_identities() {
        let f = Fixture::new(); fs::create_dir(f.0.join("tracks")).unwrap();
        let bin = f.raw("tracks/Disc.bin",0,2);
        let cue = f.0.join("Disc.cue");
        fs::write(&cue,"FILE \"tracks/Disc.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n").unwrap();
        let mut patch = b"SBI\0\x00\x02\x00\x01".to_vec(); patch.extend_from_slice(&Q[..10]);
        fs::write(cue.with_extension("sbi"),&patch).unwrap();
        patch[8] = 0x11; fs::write(bin.with_extension("sbi"),patch).unwrap();
        let info = inspect(&cue,0).unwrap();
        assert_eq!(info.sources.len(),2);
        assert_eq!(info.sources[0].name,info.sources[1].name);
        assert_ne!(info.sources[0].id,info.sources[1].id);
        assert_ne!(info.sources[0].record.as_ref().unwrap().q,info.sources[1].record.as_ref().unwrap().q);
    }

    #[test]
    fn container_subchannels_are_disc_addressed_not_indexed_from_the_container_file() {
        let f = Fixture::new(); let raw = f.raw("data.bin",10,3);
        let nrg = f.0.join("Disc.nrg");
        let mut bytes = vec![0;2352]; bytes.extend(fs::read(raw).unwrap());
        let chunk_offset = bytes.len() as u32;
        let mut dao = vec![0;52]; dao[44..48].copy_from_slice(&2352u32.to_be_bytes());
        bytes.extend(b"DAOI"); bytes.extend(52u32.to_be_bytes()); bytes.extend(dao);
        bytes.extend(chunk_offset.to_be_bytes()); bytes.extend(b"NERO");
        fs::write(&nrg, bytes).unwrap();
        let mut sub = vec![0;20*96]; sub[10*96+12..10*96+24].copy_from_slice(&Q);
        fs::write(nrg.with_extension("sub"),sub).unwrap();
        let info = inspect(&nrg,0).unwrap();
        assert_eq!(info.disc_lba,Some(10));
        assert_eq!(info.sources[0].record.as_ref().unwrap().q,Q.map(Some));
        assert!(info.sources[0].issue.is_none());
    }

    #[test]
    #[ignore = "requires DX_SBI_DIR containing the supplied PS1/PC/Mac SBI collection"]
    fn supplied_sbi_collection_parses_and_preserves_every_stored_byte() {
        let root = PathBuf::from(std::env::var("DX_SBI_DIR").unwrap());
        let mut count = 0;
        let fixture = Fixture::new();
        let image = fixture.raw("Disc.bin",0,2);
        for entry in fs::read_dir(root).unwrap() {
            let p = entry.unwrap().path();
            if ext_of(&p) != "sbi" { continue; }
            let bytes = fs::read(&p).unwrap();
            let Stored::Patch(records) = parse_patch(&bytes,true).unwrap_or_else(|e| panic!("{}: {e}",p.display())) else { panic!() };
            if count == 0 {
                // Exercise the same inspection path the UI calls using a real
                // supplied patch, including pre-LBA-zero PC/Mac records.
                fs::write(image.with_extension("sbi"),&bytes).unwrap();
                for (address,q) in [records.first_key_value().unwrap(),records.last_key_value().unwrap()] {
                    let report = inspect_record(&image,0,Some(*address)).unwrap();
                    assert_eq!(report.disc_lba,Some(*address));
                    assert_eq!(report.record_only, !(0..2).contains(address));
                    assert_eq!(report.sources[0].record.as_ref().unwrap().q,*q);
                }
            }
            // Independently walk source records and compare each stored byte,
            // including the checksum bytes that SBI intentionally omits.
            let mut pos = 4;
            while pos < bytes.len() {
                let msf = &bytes[pos..pos+3];
                let dec = |v: u8| i64::from(v >> 4)*10 + i64::from(v & 15);
                let address = (dec(msf[0])*60 + dec(msf[1]))*75 + dec(msf[2]) - 150;
                let (start,len) = match bytes[pos+3] { 1 => (0,10), 2 => (3,3), 3 => (7,3), _ => panic!() };
                let q = &records[&address];
                assert_eq!(q[start..start+len],bytes[pos+4..pos+4+len].iter().copied().map(Some).collect::<Vec<_>>(),"{}",p.display());
                assert_eq!(&q[10..], &[None,None]);
                pos += 4+len;
            }
            count += 1;
        }
        assert!(count>0); println!("Verified {count} SBI files.");
    }

    #[test]
    #[ignore = "requires DX_LSD and DX_SBI referring to matching real LSD/SBI sidecars"]
    fn real_lsd_and_independent_sbi_agree_on_addresses_and_recorded_q_bytes() {
        let lsd = fs::read(std::env::var("DX_LSD").unwrap()).unwrap();
        let sbi = fs::read(std::env::var("DX_SBI").unwrap()).unwrap();
        let Stored::Patch(lsd) = parse_patch(&lsd,false).unwrap() else { panic!() };
        let Stored::Patch(sbi) = parse_patch(&sbi,true).unwrap() else { panic!() };
        assert!(!lsd.is_empty());
        assert_eq!(lsd.keys().collect::<Vec<_>>(), sbi.keys().collect::<Vec<_>>());
        for (address, q) in lsd.iter() {
            assert_eq!(q[..10], sbi[address][..10]);
            assert!(q[10..].iter().all(Option::is_some));
            assert!(sbi[address][10..].iter().all(Option::is_none));
        }
        println!("Verified {} matching LSD/SBI records.",lsd.len());
    }

    #[test]
    #[ignore = "requires DX_CCD and adjacent VALIDATION_REFERENCE.json from the independent Python scan"]
    fn real_clonecd_subchannels_match_independent_scan_at_track_boundaries() {
        let image = PathBuf::from(std::env::var("DX_CCD").unwrap());
        let reference: serde_json::Value = serde_json::from_slice(&fs::read(image.parent().unwrap().join("VALIDATION_REFERENCE.json")).unwrap()).unwrap();
        let samples = reference["samples"].as_array().unwrap();
        assert!(!samples.is_empty());
        for sample in samples {
            let sector = sample["sector"].as_u64().unwrap();
            let report = inspect(&image,sector).unwrap();
            assert_eq!(report.disc_lba,Some(sector as i64));
            assert!(!report.record_only);
            let source = report.sources.iter().find(|s| s.name == reference["sub"].as_str().unwrap()).unwrap();
            assert_eq!(source.records,reference["sectors"].as_u64().unwrap());
            assert!(source.issue.is_none(),"{:?}",source.issue);
            let record = source.record.as_ref().unwrap();
            assert_eq!(serde_json::to_value(&record.q).unwrap(),sample["q"],"sector {sector}");
            assert_eq!(serde_json::to_value(record.calculated_crc).unwrap(),sample["calculated_crc"]);
            assert_eq!(serde_json::to_value(record.recorded_crc).unwrap(),sample["recorded_crc"]);
            assert_eq!(serde_json::to_value(record.crc_valid).unwrap(),sample["crc_valid"]);
            assert_eq!(serde_json::to_value(record.timing_matches).unwrap(),sample["timing_matches"]);
            assert_eq!(record.channels.as_ref().unwrap().len(),8);
        }
        println!("Verified {} real SUB samples against independent scan: {}",samples.len(),image.display());
    }
}
