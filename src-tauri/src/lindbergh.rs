//! Lindbergh installation DVDs wrap ordinary ISO 9660 volumes in a Sega layout.
//! Payload files remain encrypted; this reader only exposes their stored bytes.
//! https://github.com/mamedev/mame/blob/master/src/mame/sega/lindbergh.cpp
use super::*;
use std::collections::BTreeMap;
use std::time::SystemTime;

const BLOCK: u64 = 2048;
// Bound discovery both before the Sega descriptor and between volumes. Jump
// over each validated volume instead of scanning encrypted game payloads.
const SEARCH_BYTES: u64 = 16 * 1024 * 1024;
const PREFIX: &str = "Lindbergh ISO 9660 · ";

#[derive(Clone, Debug)]
pub(super) struct Volume {
    pub base: u64,
    pub blocks: u64,
    pub id: String,
    index: usize,
}
impl Volume {
    pub fn label(&self) -> String {
        // View labels are also extraction subfolder names. Never interpolate
        // untrusted volume metadata as a path or Windows-reserved character.
        let safe: String = self.id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-') { c } else { '_' }).collect();
        format!("{PREFIX}{} ({safe})", self.index + 1)
    }
    pub fn read(&self, path: &Path, lba: u64) -> Option<[u8; 2048]> {
        if lba >= self.blocks { return None; }
        sector(&mut File::open(path).ok()?, self.base + lba * BLOCK)
    }
    pub fn open(&self, path: &Path) -> Result<ISO9660<VolumeReader>, String> {
        let file = File::open(path).map_err(|e| format!("Cannot open Lindbergh image: {e}"))?;
        ISO9660::new(VolumeReader { file, base: self.base, bytes: self.blocks * BLOCK })
            .map_err(|e| format!("Invalid Lindbergh volume: {e}"))
    }
}

pub(super) struct VolumeReader { file: File, base: u64, bytes: u64 }
impl ISO9660Reader for VolumeReader {
    fn read_at(&mut self, buf: &mut [u8], lba: u64) -> io::Result<usize> {
        let offset = lba.checked_mul(BLOCK).ok_or_else(|| io::Error::other("Lindbergh LBA overflow"))?;
        if offset >= self.bytes { return Ok(0); }
        self.file.seek(SeekFrom::Start(self.base + offset))?;
        let size = (self.bytes - offset).min(buf.len() as u64) as usize;
        self.file.read(&mut buf[..size])
    }
}

fn both32(bytes: &[u8]) -> Option<u64> {
    let a = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
    let b = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?);
    (a == b).then_some(a as u64)
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim_matches([' ', '\0']).to_string()
}
fn sector(file: &mut File, offset: u64) -> Option<[u8; 2048]> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = [0; 2048];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}
fn validate(file: &mut File, pvd: &[u8], at: u64, len: u64) -> Option<Volume> {
    if pvd.get(..7)? != b"\x01CD001\x01" || pvd.get(128..132)? != [0, 8, 8, 0] { return None; }
    let base = at.checked_sub(16 * BLOCK)?;
    let blocks = both32(pvd.get(80..88)?)?;
    let end = base.checked_add(blocks.checked_mul(BLOCK)?)?;
    if blocks < 18 || end > len { return None; }
    let root = pvd.get(156..190)?;
    if root[0] != 34 || root[25] & 2 == 0 || root[32..34] != [1, 0] { return None; }
    let root_lba = both32(&root[2..10])?;
    let root_bytes = both32(&root[10..18])?;
    if root_lba < 18 || root_bytes < 34 || root_lba.checked_mul(BLOCK)?.checked_add(root_bytes)? > blocks * BLOCK { return None; }
    let dot = sector(file, base + root_lba * BLOCK)?;
    // Rock Ridge system-use fields can make the directory's dot record longer
    // than the fixed 34-byte root record in the PVD. Compare its allocation,
    // not its record length (the real Lindbergh sample uses 136 bytes here).
    if dot[0] < 34 || dot[1] != root[1] || dot[2..18] != root[2..18]
        || dot[25] & 2 == 0 || dot[32..34] != [1, 0] { return None; }
    let mut terminated = false;
    for lba in 17..blocks.min(80) {
        let descriptor = sector(file, base + lba * BLOCK)?;
        if &descriptor[1..7] != b"CD001\x01" { return None; }
        if descriptor[0] == 255 { terminated = true; break; }
        if !matches!(descriptor[0], 0..=3) { return None; }
    }
    if !terminated { return None; }
    Some(Volume { base, blocks, id: text(pvd.get(40..72)?), index: 0 })
}

fn find(file: &mut File, start: u64, len: u64, sega: bool) -> Option<Volume> {
    let start = start.max(16 * BLOCK).div_ceil(BLOCK) * BLOCK;
    let end = start.saturating_add(SEARCH_BYTES).min(len);
    if end <= start { return None; }
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![0; (end - start) as usize];
    file.read_exact(&mut bytes).ok()?;
    for offset in (0..bytes.len().saturating_sub(2047)).step_by(2048) {
        let pvd = &bytes[offset..offset + 2048];
        if &pvd[..7] != b"\x01CD001\x01" { continue; }
        if sega && (text(&pvd[8..40]) != "SEGA_LINDBERGH" || !text(&pvd[40..72]).starts_with("LBG-DVDROM-")) { continue; }
        if let Some(volume) = validate(file, pvd, start + offset as u64, len) { return Some(volume); }
    }
    None
}

struct Cached { len: u64, modified: Option<SystemTime>, volumes: Vec<Volume> }
static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Cached>>> = OnceLock::new();

pub(super) fn discover(path: &Path) -> Vec<Volume> {
    if !matches!(ext_of(path).as_str(), "iso" | "img" | "bin") { return Vec::new(); }
    let Ok(meta) = fs::metadata(path) else { return Vec::new(); };
    if !meta.is_file() || meta.len() < 18 * BLOCK { return Vec::new(); }
    let modified = meta.modified().ok();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(locked) = cache.lock() {
        if let Some(hit) = locked.get(path).filter(|hit| hit.len == meta.len() && hit.modified == modified) {
            return hit.volumes.clone();
        }
    }
    let mut volumes = Vec::new();
    if let Ok(mut file) = File::open(path) {
        // Ordinary ISO images need no prefix search (nor a scan through files
        // which could themselves contain a Lindbergh image).
        let standard_iso = sector(&mut file, 16 * BLOCK)
            .is_some_and(|pvd| &pvd[1..7] == b"CD001\x01" && text(&pvd[8..40]) != "SEGA_LINDBERGH");
        if let Some(first) = (!standard_iso).then(|| find(&mut file, 16 * BLOCK, meta.len(), true)).flatten() {
            volumes.push(first);
            while volumes.len() < 32 {
                let last = volumes.last().unwrap();
                let end = last.base + last.blocks * BLOCK;
                let Some(mut next) = find(&mut file, end + 16 * BLOCK, meta.len(), false) else { break };
                if next.base < end { break; }
                next.index = volumes.len();
                volumes.push(next);
            }
        }
    }
    if let Ok(mut locked) = cache.lock() {
        if locked.len() >= 128 { locked.clear(); }
        locked.insert(path.to_path_buf(), Cached { len: meta.len(), modified, volumes: volumes.clone() });
    }
    volumes
}

pub(super) fn labels(path: &Path) -> Vec<String> {
    let mut volumes = discover(path);
    // Open the content volume by default, while retaining the Sega metadata view.
    volumes.sort_by_key(|v| std::cmp::Reverse(v.blocks));
    volumes.iter().map(Volume::label).collect()
}
pub(super) fn select(path: &Path, filesystem: Option<&str>) -> Result<Option<Volume>, String> {
    if filesystem.is_some_and(|name| !name.starts_with(PREFIX)) { return Ok(None); }
    let volumes = discover(path);
    match filesystem {
        Some(name) => volumes.into_iter().find(|v| v.label() == name).map(Some).ok_or_else(|| "Lindbergh volume is no longer available.".into()),
        None => Ok(volumes.into_iter().max_by_key(|v| v.blocks)),
    }
}

pub(super) fn list(path: &Path, volume: &Volume, dir: &str) -> Result<Vec<DiscEntry>, String> {
    let mut entries = collect_entries(&volume.open(path)?, dir, NameSpace::Iso, false)?;
    for entry in &mut entries {
        entry.lba = u32::try_from(entry.lba as u64 + volume.base / BLOCK).map_err(|_| "Lindbergh LBA overflow")?;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("dx-lindbergh-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn image(&self) -> PathBuf { self.0.join("image.iso") }
    }
    impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    fn iso(sega: bool) -> Vec<u8> {
        let mut bytes = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso")).unwrap();
        if sega {
            bytes[32768 + 8..32768 + 40].fill(b' ');
            bytes[32768 + 8..32768 + 22].copy_from_slice(b"SEGA_LINDBERGH");
            bytes[32768 + 40..32768 + 72].fill(b' ');
            bytes[32768 + 40..32768 + 55].copy_from_slice(b"LBG-DVDROM-TEST");
        }
        bytes
    }
    fn wrapped(temp: &Temp) -> (PathBuf, u64, u64) {
        let sega = iso(true);
        let game = iso(false);
        // Deliberately unlike the real sample's offsets. Insert an entire ISO
        // inside the prefix too; without the Sega identifier it must be ignored.
        let base = (game.len() as u64).div_ceil(BLOCK) * BLOCK + 7 * BLOCK;
        let second = base + sega.len() as u64 + 19 * BLOCK;
        let mut bytes = vec![0x59; second as usize + game.len()];
        bytes[..game.len()].copy_from_slice(&game);
        // Avoid a standard ISO at zero, while retaining an embedded false lead.
        bytes[32769] = 0;
        bytes[base as usize..base as usize + sega.len()].copy_from_slice(&sega);
        bytes[second as usize..].copy_from_slice(&game);
        fs::write(temp.image(), bytes).unwrap();
        (temp.image(), base, second)
    }

    #[test]
    fn embedded_volumes_use_relative_reads_and_absolute_listing_lbas() {
        let temp = Temp::new();
        let (path, first, second) = wrapped(&temp);
        let volumes = discover(&path);
        assert_eq!(volumes.iter().map(|v| v.base).collect::<Vec<_>>(), [first, second]);
        assert_eq!(labels(&path).len(), 2);
        let label = volumes[1].label();
        let found = get_disc_filesystems(path.to_string_lossy().into_owned()).unwrap();
        assert!(found.contains(&label));
        let listed = list_disc_contents(path.to_string_lossy().into_owned(), "/".into(), Some(label.clone()), false).unwrap();
        let reference = ISO9660::new(std::io::Cursor::new(iso(false))).unwrap();
        let expected = collect_entries(&reference, "/", NameSpace::Iso, false).unwrap();
        assert_eq!(listed.len(), expected.len());
        for (actual, expected) in listed.iter().zip(expected) {
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.lba as u64, expected.lba as u64 + second / BLOCK);
        }
        assert_eq!(disc_volume_label(path.to_string_lossy().into_owned(), Some(label.clone())), volumes[1].id);
        assert_eq!(disc_info::collect_info(&path.to_string_lossy()).unwrap().sections.len(), 2);
        let dates = disc_date_report(path.to_string_lossy().into_owned(), Some(label)).unwrap();
        assert!(!dates.pvd_created.is_empty());
        assert!(dates.entries_scanned > 0);
        assert!(select(&path, Some("Lindbergh ISO 9660 · 99 (missing)")).is_err());
        assert!(select(&path, Some("ISO 9660")).unwrap().is_none());
        let volume = &volumes[1];
        let mut reader = VolumeReader { file: File::open(&path).unwrap(), base: second, bytes: volume.blocks * BLOCK };
        assert_eq!(reader.read_at(&mut [0; 2048], volume.blocks).unwrap(), 0);
        assert!(reader.read_at(&mut [0; 2048], u64::MAX).is_err());
    }

    #[test]
    fn rejects_unmarked_truncated_and_inconsistent_volumes_and_invalidates_cache() {
        let temp = Temp::new();
        let ordinary = iso(false);
        let mut bytes = vec![0; 31 * 2048];
        bytes.extend_from_slice(&ordinary);
        fs::write(temp.image(), &bytes).unwrap();
        assert!(discover(&temp.image()).is_empty());
        let (path, base, _) = wrapped(&temp);
        assert_eq!(discover(&path).len(), 2);
        // Changed length invalidates the cached successful discovery; reject a
        // partial first volume rather than fall through to the unmarked second.
        fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(base + 18 * BLOCK).unwrap();
        assert!(discover(&path).is_empty());
        let (path, base, _) = wrapped(&temp);
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(base + 16 * BLOCK + 84)).unwrap();
        file.write_all(&0u32.to_be_bytes()).unwrap();
        // Make cache invalidation independent of filesystem timestamp precision.
        file.set_len(file.metadata().unwrap().len() + 1).unwrap();
        assert!(discover(&path).is_empty());
    }

    #[test]
    fn sega_volume_at_zero_and_extended_dot_records_are_supported() {
        let temp = Temp::new();
        let mut bytes = iso(true);
        let root = u32::from_le_bytes(bytes[32768 + 158..32768 + 162].try_into().unwrap()) as usize * 2048;
        let old = bytes[root] as usize;
        bytes.copy_within(root + old..root + 2048 - (136 - old), root + 136);
        bytes[root + old..root + 136].fill(0);
        bytes[root] = 136;
        fs::write(temp.image(), bytes).unwrap();
        let volumes = discover(&temp.image());
        assert_eq!(volumes.len(), 1);
        assert!(get_disc_filesystems(temp.image().to_string_lossy().into_owned()).unwrap()[0].starts_with(PREFIX));
        let hostile = Volume { base: 0, blocks: 64, id: "../../folder\\name:*?".into(), index: 0 };
        assert!(!hostile.label().contains(['/', '\\', ':', '*', '?']));
    }

    #[test]
    #[ignore]
    fn real_lindbergh_lists_extracts_hashes_and_locates_the_stored_payloads() {
        use sha2::{Digest, Sha256};
        let image = std::env::var("DX_IMG").expect("set DX_IMG to Ami-Gyo Rev C ISO");
        let path = Path::new(&image);
        let volumes = discover(path);
        assert_eq!(volumes.len(), 2);
        assert_eq!((volumes[0].base, volumes[1].base), (0x280000, 0x388000));
        let volume = &volumes[1];
        let label = volume.label();
        let entries = list_disc_contents(image.clone(), "/".into(), Some(label.clone()), false).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.to_ascii_lowercase()).collect();
        // Independent 7-Zip listing of the offset volume, recorded during research.
        assert_eq!(names, ["disk0.img", "disk1.img", "disk8.img", "frontend", "su1.dat", "su2.dat"]);
        let expected = [(31u64,372736u32),(213,517808128),(253049,42979328),(274035,2695),(274037,32475657),(289895,328302)];
        let temp = Temp::new();
        let fs = volume.open(path).unwrap();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        extract_dir_tree(IsoExtract { fs: &fs, ns: NameSpace::Iso }, &cancel, "/", &temp.0.to_string_lossy()).unwrap();
        for (entry, (relative, size)) in entries.iter().zip(expected) {
            assert_eq!(entry.size, size);
            assert_eq!(entry.lba as u64, relative + volume.base / BLOCK);
            let mut raw = File::open(path).unwrap();
            raw.seek(SeekFrom::Start(volume.base + relative * BLOCK)).unwrap();
            let mut raw = raw.take(size as u64);
            let mut direct = Sha256::new();
            io::copy(&mut raw, &mut direct).unwrap();
            let mut extracted = Sha256::new();
            io::copy(&mut File::open(temp.0.join(&entry.name)).unwrap(), &mut extracted).unwrap();
            assert_eq!(direct.finalize(), extracted.finalize(), "{}", entry.name);
            let mut location = None;
            dispatch_single_file(image.clone(), format!("/{}", entry.name), Some(label.clone()), &mut entry_tools::Output::Location(&mut location)).unwrap();
            let location = serde_json::to_value(location.unwrap()).unwrap();
            assert_eq!(location["ranges"][0]["start_lba"].as_u64(), Some(entry.lba as u64));
        }
        let mut stream = Vec::new();
        dispatch_single_file(image.clone(), format!("/{}", entries[3].name), Some(label), &mut entry_tools::Output::Stream(&mut stream)).unwrap();
        assert_eq!(stream, fs::read(temp.0.join(&entries[3].name)).unwrap());
        let dates = disc_date_report(image.clone(), None).unwrap();
        // Recorded calendar time; unlike 7-Zip's local display, do not convert
        // the ISO's +09:00 timestamps into this machine's timezone.
        assert_eq!(dates.latest_file_date, "2006-05-18 15:56:44");
        let info = disc_info::collect_info(&image).unwrap();
        assert_eq!(info.sections.len(), 2);
        println!("Two Lindbergh volumes, six payloads, extraction hashes and absolute LBAs verified.");
    }
}
