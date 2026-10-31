//! On-demand file hashes and allocation metadata. Hashing shares the extraction
//! dispatcher, replacing its destination with a writer rather than a temp file.
use super::*;
use md5::{Digest, Md5};
use sha1::Sha1;
use sha2::Sha256;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// Open destinations lazily: lookup/read failures must not truncate an existing
// file before the old extraction path would have opened it. Empty files still
// get created after a successful copy.
pub(crate) fn write_to_path(path: &str, copy: impl FnOnce(&mut dyn Write) -> Result<(), String>) -> Result<(), String> {
    struct LazyFile<'a> { path: &'a str, file: Option<File> }
    impl LazyFile<'_> {
        fn open(&mut self) -> io::Result<&mut File> {
            if self.file.is_none() { self.file = Some(File::create(self.path)?); }
            Ok(self.file.as_mut().unwrap())
        }
    }
    impl Write for LazyFile<'_> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> { self.open()?.write(data) }
        fn flush(&mut self) -> io::Result<()> { self.open()?.flush() }
    }
    let mut out = LazyFile { path, file: None };
    copy(&mut out)?;
    out.open().map_err(|e| format!("Cannot create output: {e}"))?;
    Ok(())
}

pub(crate) trait CopyFile {
    fn disk(&mut self, path: &str, dest: &str) -> Result<(), String>;
    fn stream(&mut self, path: &str, out: &mut dyn Write) -> Result<(), String>;
    fn location(&mut self, _path: &str) -> Result<Location, String> { Err(UNSUPPORTED.into()) }
}
macro_rules! copy_file {
    ($ty:ty $(, $($gen:tt)+)?) => {
        impl$(<$($gen)+>)? CopyFile for $ty {
            fn disk(&mut self, path: &str, dest: &str) -> Result<(), String> { self.extract_file(path, dest) }
            fn stream(&mut self, path: &str, out: &mut dyn Write) -> Result<(), String> { self.copy_file_to(path, out) }
        }
    };
}
copy_file!(cdi_filesystem::CdiFs);
copy_file!(pce_filesystem::PceFs);
impl CopyFile for hfs_filesystem::HfsFs {
    fn disk(&mut self, path: &str, dest: &str) -> Result<(), String> { self.extract_file(path, dest) }
    fn stream(&mut self, path: &str, out: &mut dyn Write) -> Result<(), String> { self.copy_file_to(path, out) }
    fn location(&mut self, path: &str) -> Result<Location, String> { self.entry_location(path) }
}
copy_file!(udf_filesystem::UdfFs);
copy_file!(fat_filesystem::FatFs<R>, R: Read + Seek);
copy_file!(fatx_filesystem::FatxFs<R>, R: Read + Seek);
copy_file!(gcm_filesystem::GcmFs<R>, R: Read + Seek);
copy_file!(threedo_filesystem::ThreeDOFs<R>, R: Read + Seek);
copy_file!(xdvdfs_filesystem::XDVDFSFs<R>, R: Read + Seek);
copy_file!(nitro_filesystem::NitroFs);
copy_file!(zip_archive::ZipArchive);
copy_file!(tar_archive::TarArchive);
copy_file!(cab_archive::CabArchive);
copy_file!(vpk_archive::VpkArchive);
copy_file!(zarchive::ZArchive);

pub(crate) enum Output<'a> {
    Disk(&'a str),
    Stream(&'a mut dyn Write),
    Location(&'a mut Option<Location>),
}
const UNSUPPORTED: &str = "Sector locations are not available for this filesystem.";
impl Output<'_> {
    pub(crate) fn iso_volume<T: ISO9660Reader>(&mut self, fs: &ISO9660<T>, path: &str, base_lba: u64) -> Result<(), String> {
        self.iso(fs, path, NameSpace::Iso)?;
        if let Self::Location(result) = self {
            if let Some(location) = result.as_mut() {
                for range in &mut location.ranges {
                    range.start_lba = range.start_lba.checked_add(base_lba).ok_or("Lindbergh LBA overflow")?;
                    range.end_lba = range.end_lba.checked_add(base_lba).ok_or("Lindbergh LBA overflow")?;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn copy(&mut self, fs: &mut impl CopyFile, path: &str) -> Result<(), String> {
        match self {
            Self::Disk(dest) => fs.disk(path, dest),
            Self::Stream(out) => fs.stream(path, *out),
            Self::Location(result) => { **result = Some(fs.location(path)?); Ok(()) },
        }
    }
    pub(crate) fn iso<T: ISO9660Reader>(&mut self, fs: &ISO9660<T>, path: &str, ns: NameSpace) -> Result<(), String> {
        match self {
            Self::Disk(dest) => extract_file_from_fs(fs, path, dest, ns),
            Self::Stream(out) => {
                let entry = if let Some(base) = path.strip_suffix(".[R]") {
                    DirectoryEntry::File(open_resource_fork(fs, base, ns)?)
                } else { fs.open_view(path, ns).map_err(|e| format!("Path error: {e}"))?.ok_or("File not found")? };
                let h = entry.header();
                if h.file_flags.bits() & 0x80 != 0 || h.file_unit_size != 0 || h.interleave_gap_size != 0 || h.extended_attribute_record_length != 0 {
                    return Err("Hashing this allocation layout is not supported.".into());
                }
                copy_file_from_fs(fs, path, *out, ns)
            },
            Self::Location(result) => { **result = Some(iso_location(fs, path, ns)?); Ok(()) }
        }
    }
    pub(crate) fn host(&mut self, src: &Path) -> Result<(), String> {
        match self {
            Self::Disk(dest) => fs::copy(src, *dest).map(|_| ()).map_err(|e| format!("Copy error: {e}")),
            Self::Stream(out) => io::copy(&mut File::open(src).map_err(|e| format!("Cannot open file: {e}"))?, *out)
                .map(|_| ()).map_err(|e| format!("Read error: {e}")),
            Self::Location(_) => Err(UNSUPPORTED.into()),
        }
    }
    pub(crate) fn el_torito(&mut self, track: &DataTrack, path: &str) -> Result<(), String> {
        match self {
            Self::Disk(dest) => el_torito_extract(track, path, dest),
            Self::Stream(out) => el_torito_extract_to(track, path, *out),
            Self::Location(_) => Err(UNSUPPORTED.into()),
        }
    }
    pub(crate) fn wiiu_gm<R: Read + Seek>(&mut self, fst: &WiiUFst, reader: &mut R, path: &str, key: &[u8; 16]) -> Result<(), String> {
        match self {
            Self::Disk(dest) => wiiu_gm_fst_extract_file(fst, reader, path, dest, key),
            Self::Stream(out) => wiiu_gm_fst_extract_file_to(fst, reader, path, *out, key),
            Self::Location(_) => Err(UNSUPPORTED.into()),
        }
    }
    pub(crate) fn wiiu_si<R: Read + Seek>(&mut self, fst: &WiiUFst, reader: &mut R, path: &str, key: Option<&[u8; 16]>) -> Result<(), String> {
        match self {
            Self::Disk(dest) => wiiu_fst_extract_file(fst, reader, path, dest, key),
            Self::Stream(out) => wiiu_fst_extract_file_to(fst, reader, path, *out, key),
            Self::Location(_) => Err(UNSUPPORTED.into()),
        }
    }
}

#[derive(Serialize, Debug)]
pub(crate) struct SectorRange {
    start_lba: u64,
    end_lba: u64,
    sector_count: u64,
    start_byte_offset: u32,
    end_byte_offset: u32,
}
#[derive(Serialize, Debug)]
pub(crate) struct Location {
    is_directory: bool,
    logical_block_size: u32,
    declared_bytes: u64,
    ranges: Vec<SectorRange>,
    sector_count: u64,
    // None when the detached viewer cannot represent this filesystem address.
    viewer_lba: Option<u64>,
    lba_basis: &'static str,
    directory_storage: &'static str,
}

impl Location {
    pub(crate) fn from_track_bytes(is_directory: bool, declared_bytes: u64, byte_ranges: Vec<(u64, u64)>, block: u64) -> Result<Self, String> {
        let mut ranges = Vec::new();
        for (start, len) in byte_ranges {
            if len == 0 { continue; }
            let end = start.checked_add(len - 1).ok_or("Allocation range overflow")?;
            ranges.push(SectorRange { start_lba: start / block, end_lba: end / block,
                sector_count: end / block - start / block + 1,
                start_byte_offset: (start % block) as u32, end_byte_offset: (end % block) as u32 });
        }
        let sector_count = unique_sector_count(&ranges);
        Ok(Self { is_directory, declared_bytes, logical_block_size: block as u32, ranges, sector_count, viewer_lba: None,
            lba_basis: "track", directory_storage: if is_directory { "catalog" } else { "directory" } })
    }
}

fn unique_sector_count(ranges: &[SectorRange]) -> u64 {
    let mut spans: Vec<_> = ranges.iter().map(|range| (range.start_lba,range.end_lba)).collect();
    spans.sort_unstable();
    let mut count = 0;
    let mut current: Option<(u64,u64)> = None;
    for (start,end) in spans {
        match current {
            Some((a,b)) if start <= b.saturating_add(1) => current = Some((a,b.max(end))),
            Some((a,b)) => { count += b-a+1; current = Some((start,end)); },
            None => current = Some((start,end)),
        }
    }
    if let Some((a,b)) = current { count += b-a+1; }
    count
}

struct Allocation { extent_loc: u32, extent_length: u32, extended_attribute_record_length: u8, file_unit_size: u8, interleave_gap_size: u8 }
fn add_extent(ranges: &mut Vec<SectorRange>, h: &Allocation, block: u64) -> Result<(), String> {
    let mut count = (h.extent_length as u64).div_ceil(block);
    let mut start = h.extent_loc as u64 + h.extended_attribute_record_length as u64;
    if h.file_unit_size == 0 && h.interleave_gap_size != 0 {
        return Err("This interleaved allocation cannot be mapped reliably.".into());
    }
    while count > 0 {
        if ranges.len() >= 100_000 { return Err("Too many sector ranges to display.".into()); }
        let n = if h.file_unit_size == 0 { count } else { count.min(h.file_unit_size as u64) };
        ranges.push(SectorRange {
            start_lba: start, end_lba: start + n - 1, sector_count: n,
            start_byte_offset: 0,
            end_byte_offset: if n == count { ((h.extent_length as u64 - 1) % block) as u32 } else { (block - 1) as u32 },
        });
        count -= n;
        start += n + h.interleave_gap_size as u64;
    }
    Ok(())
}
fn iso_location<T: ISO9660Reader>(fs: &ISO9660<T>, path: &str, ns: NameSpace) -> Result<Location, String> {
    let entry = if let Some(base) = path.strip_suffix(".[R]") {
        DirectoryEntry::File(open_resource_fork(fs, base, ns)?)
    } else {
        fs.open_view(path, ns).map_err(|e| format!("Path error: {e}"))?.ok_or("File not found")?
    };
    let is_directory = matches!(&entry, DirectoryEntry::Directory(_));
    let mut headers = vec![entry.header().clone()];
    if !is_directory && entry.header().file_flags.bits() & 0x80 != 0 {
        // ISO multi-extent files repeat a directory record. Collect every part
        // in directory order; the final record clears the continuation flag.
        let base = path.strip_suffix(".[R]").unwrap_or(path);
        let (parent, _) = base.rsplit_once('/').unwrap_or(("/", base));
        let parent = if parent.is_empty() { "/" } else { parent };
        let dir = match fs.open_view(parent, ns).map_err(|e| format!("Path error: {e}"))? {
            Some(DirectoryEntry::Directory(d)) => d,
            _ => return Err("Directory not found".into()),
        };
        headers.clear();
        let mut collecting = false;
        let mut complete = false;
        for part in dir.contents() {
            let part = part.map_err(|e| format!("Read error: {e}"))?;
            let same = part.identifier() == entry.identifier()
                && part.header().file_flags.bits() & 0x04 == entry.header().file_flags.bits() & 0x04
                && match (&entry, &part) { (DirectoryEntry::File(a), DirectoryEntry::File(b)) => a.version == b.version, _ => false };
            if !collecting {
                if !same || part.header().extent_loc != entry.header().extent_loc { continue; }
                collecting = true;
            }
            if !same { return Err("Incomplete multi-extent allocation.".into()); }
            if headers.len() >= 100_000 { return Err("Too many sector ranges to display.".into()); }
            headers.push(part.header().clone());
            if part.header().file_flags.bits() & 0x80 == 0 { complete = true; break; }
        }
        if !complete { return Err("Incomplete multi-extent allocation.".into()); }
    }
    let mut result = Location { is_directory, logical_block_size: fs.block_size() as u32,
        declared_bytes: 0, ranges: Vec::new(), sector_count: 0, viewer_lba: None, lba_basis: "disc", directory_storage: "directory" };
    for h in headers {
        result.declared_bytes += h.extent_length as u64;
        add_extent(&mut result.ranges, &Allocation {
            extent_loc: h.extent_loc, extent_length: h.extent_length,
            extended_attribute_record_length: h.extended_attribute_record_length,
            file_unit_size: h.file_unit_size, interleave_gap_size: h.interleave_gap_size,
        }, result.logical_block_size as u64)?;
    }
    result.sector_count = unique_sector_count(&result.ranges);
    Ok(result)
}

#[tauri::command]
pub(crate) async fn sector_location(image_path: String, file_path: String, filesystem: Option<String>) -> Result<Location, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut result = None;
        dispatch_single_file(image_path.clone(), file_path, filesystem, &mut Output::Location(&mut result))?;
        let mut result = result.ok_or(UNSUPPORTED)?;
        if let Some(first) = result.ranges.first() {
            // The viewer addresses descriptor images relative to the first data
            // track. Do not pass a disc-absolute extent straight to that view.
            let lower = image_path.to_lowercase();
            let track = if lower.ends_with(".cue") { parse_cue_all_data_tracks(Path::new(&image_path)).ok().and_then(|t| t.into_iter().next()) }
                else if matches!(ext_of(Path::new(&image_path)).as_str(), "mds" | "nrg" | "ccd" | "cdi" | "gdi" | "b5t" | "b6t" | "cif") { parse_track(&image_path).ok() }
                else { None };
            result.viewer_lba = if let Some(t) = track {
                if result.lba_basis == "track" {
                    // HFS allocation bytes belong to the reader's chosen track.
                    // Only jump if the viewer is looking at that same track.
                    parse_track(&image_path).ok().filter(|selected| selected.bin_path == t.bin_path && selected.track_offset == t.track_offset)
                        .map(|_| first.start_lba).filter(|lba| t.sector_count == 0 || *lba < t.sector_count)
                } else {
                    first.start_lba.checked_sub(t.lba_offset).filter(|lba| t.sector_count == 0 || *lba < t.sector_count)
                }
            } else if !matches!(ext_of(Path::new(&image_path)).as_str(), "skeleton" | "scram" | "sdram" | "sbram" | "chd") && !lower.ends_with(".skeleton.zst") {
                Some(first.start_lba)
            } else { None };
        }
        Ok(result)
    }).await.map_err(|e| format!("Sector location failed: {e}"))?
}

#[derive(Default)]
pub(crate) struct HashJobs(Mutex<HashRegistry>);
#[derive(Default)]
struct HashRegistry {
    active: Option<(String, Arc<AtomicBool>)>,
    // IPC cancellation can reach the backend before its async start command is
    // polled. Keep a bounded set of early requests, including harmless late
    // cleanup requests from dialogs that have already completed.
    early_cancels: VecDeque<String>,
}
impl HashRegistry {
    fn begin(&mut self, id: String) -> Result<Arc<AtomicBool>, String> {
        if self.active.is_some() { return Err("A hash calculation is already running.".into()); }
        let cancelled = self.early_cancels.iter().any(|saved| saved == &id);
        self.early_cancels.retain(|saved| saved != &id);
        let flag = Arc::new(AtomicBool::new(cancelled));
        self.active = Some((id, flag.clone()));
        Ok(flag)
    }
    fn cancel(&mut self, id: &str) {
        if let Some((active_id, flag)) = &self.active {
            if active_id == id { flag.store(true, Ordering::Relaxed); return; }
        }
        if !self.early_cancels.iter().any(|saved| saved == id) {
            if self.early_cancels.len() >= 32 { self.early_cancels.pop_front(); }
            self.early_cancels.push_back(id.into());
        }
    }
    fn finish(&mut self, id: &str) {
        if self.active.as_ref().is_some_and(|(active_id, _)| active_id == id) { self.active = None; }
    }
}
#[derive(Serialize, Debug)]
pub(crate) struct HashResult {
    bytes: u64,
    crc32: String,
    md5: String,
    sha1: String,
    sha256: String,
}
struct HashWriter<'a> {
    crc: crc32fast::Hasher, md5: Md5, sha1: Sha1, sha256: Sha256,
    bytes: u64, cancel: Arc<AtomicBool>, last: Instant,
    progress: &'a mut dyn FnMut(u64),
}
impl<'a> HashWriter<'a> {
    fn new(cancel: Arc<AtomicBool>, progress: &'a mut dyn FnMut(u64)) -> Self {
        Self { crc: crc32fast::Hasher::new(), md5: Md5::new(), sha1: Sha1::new(), sha256: Sha256::new(),
            bytes: 0, cancel, last: Instant::now(), progress }
    }
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) { Err(io::Error::other("Hash calculation cancelled.")) } else { Ok(()) }
    }
    fn finish(self) -> Result<HashResult, String> {
        self.check().map_err(|e| e.to_string())?;
        Ok(HashResult { bytes: self.bytes, crc32: format!("{:08X}", self.crc.finalize()),
            md5: format!("{:x}", self.md5.finalize()), sha1: format!("{:x}", self.sha1.finalize()), sha256: format!("{:x}", self.sha256.finalize()) })
    }
}
impl Write for HashWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        for bytes in data.chunks(64 * 1024) {
            self.check()?;
            self.crc.update(bytes); self.md5.update(bytes); self.sha1.update(bytes); self.sha256.update(bytes);
            self.bytes += bytes.len() as u64;
            if self.last.elapsed() >= Duration::from_millis(100) { (self.progress)(self.bytes); self.last = Instant::now(); }
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> { self.check() }
}

fn hash_file(image: String, path: String, filesystem: Option<String>, mode: u8, cancel: Arc<AtomicBool>, progress: &mut dyn FnMut(u64)) -> Result<HashResult, String> {
    if cancel.load(Ordering::Relaxed) { return Err("Hash calculation cancelled.".into()); }
    if mode > XA_RAW { return Err("Invalid XA extraction mode.".into()); }
    let (parent, leaf) = path.rsplit_once('/').unwrap_or(("/", &path));
    let list = list_disc_contents(image.clone(), if parent.is_empty() { "/".into() } else { parent.into() }, filesystem.clone(), true)?;
    let entry = list.iter().find(|e| e.name == leaf).ok_or("File not found")?;
    if entry.is_dir { return Err("Hash calculation is available for files only.".into()); }
    let mut writer = HashWriter::new(cancel, progress);
    writer.check().map_err(|e| e.to_string())?;
    // Same worker thread as the synchronous readers. Restore even on a read error.
    struct XaReset(u8);
    impl Drop for XaReset { fn drop(&mut self) { XA_MODE.with(|c| c.set(self.0)); } }
    let _reset = XaReset(XA_MODE.with(|c| c.replace(mode)));
    dispatch_single_file(image, path, filesystem, &mut Output::Stream(&mut writer))?;
    (writer.progress)(writer.bytes);
    writer.finish()
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // IPC exposes the file selection and per-job controls.
pub(crate) async fn calculate_file_hashes(app: tauri::AppHandle, jobs: tauri::State<'_, HashJobs>, job_id: String,
    image_path: String, file_path: String, filesystem: Option<String>, xa_mode: u8, progress: tauri::ipc::Channel<u64>) -> Result<HashResult, String> {
    let cancel = jobs.0.lock().map_err(|e| e.to_string())?.begin(job_id.clone())?;
    tauri::async_runtime::spawn_blocking(move || {
        struct Cleanup(tauri::AppHandle, String);
        impl Drop for Cleanup { fn drop(&mut self) { if let Ok(mut jobs) = self.0.state::<HashJobs>().0.lock() { jobs.finish(&self.1); } } }
        let _cleanup = Cleanup(app, job_id);
        hash_file(image_path, file_path, filesystem, xa_mode, cancel, &mut |bytes| { let _ = progress.send(bytes); })
    }).await.map_err(|e| format!("Hash calculation failed: {e}"))?
}
#[tauri::command]
pub(crate) fn cancel_file_hashes(jobs: tauri::State<'_, HashJobs>, job_id: String) {
    if let Ok(mut jobs) = jobs.0.lock() { jobs.cancel(&job_id); }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cancel() -> Arc<AtomicBool> { Arc::new(AtomicBool::new(false)) }
    fn hash(bytes: &[u8]) -> HashResult {
        let mut progress = |_| {};
        let mut writer = HashWriter::new(cancel(), &mut progress);
        for part in bytes.chunks(17) { writer.write_all(part).unwrap(); }
        writer.finish().unwrap()
    }
    #[test]
    fn cancellation_before_registration_and_late_cleanup_are_isolated() {
        let mut jobs = HashRegistry::default();
        jobs.cancel("early");
        assert!(jobs.begin("early".into()).unwrap().load(Ordering::Relaxed));
        jobs.finish("early");
        let active = jobs.begin("next".into()).unwrap();
        jobs.cancel("early"); jobs.finish("early");
        assert!(!active.load(Ordering::Relaxed));
        assert!(jobs.begin("overlap".into()).is_err());
        jobs.cancel("next"); assert!(active.load(Ordering::Relaxed)); jobs.finish("next");
        for n in 0..100 { jobs.cancel(&format!("late-{n}")); }
        assert_eq!(jobs.early_cancels.len(),32);
        assert!(!jobs.begin("fresh".into()).unwrap().load(Ordering::Relaxed));
    }
    #[test]
    fn standard_vectors_and_empty_file() {
        let result = hash(b"123456789");
        assert_eq!(result.bytes, 9); assert_eq!(result.crc32, "CBF43926");
        assert_eq!(result.md5, "25f9e794323b453885f5181f1b624d0b");
        assert_eq!(result.sha1, "f7c3bc1d808e04732adf679965ccc34ca7ae3441");
        assert_eq!(result.sha256, "15e2b0d3c33891ebb0f1ef609ec419420c20e320ce94c65fbc8c3312448eb225");
        let empty = hash(b""); assert_eq!(empty.crc32, "00000000");
        assert_eq!(empty.md5, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(empty.sha1, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(empty.sha256, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
    #[test]
    fn progress_and_cancellation_during_a_large_write() {
        let cancel = cancel(); let mut reported = 0; let flag = cancel.clone();
        let mut progress = |bytes| { reported = bytes; flag.store(true, Ordering::Relaxed); };
        let mut writer = HashWriter::new(cancel, &mut progress);
        writer.last = Instant::now() - Duration::from_secs(1);
        assert!(writer.write_all(&vec![0; 256 * 1024]).is_err());
        assert_eq!(writer.bytes, 64 * 1024); assert!(writer.finish().is_err()); assert_eq!(reported, 64 * 1024);
    }
    #[test]
    fn hfs_byte_ranges_preserve_partial_sector_offsets() {
        let location = Location::from_track_bytes(false, 1027, vec![(512,1024),(4096,3)], 2048).unwrap();
        assert_eq!(location.lba_basis,"track");
        assert_eq!(location.sector_count,2);
        let shared = Location::from_track_bytes(false,200,vec![(512,100),(1024,100)],2048).unwrap();
        assert_eq!(shared.ranges.len(),2); assert_eq!(shared.sector_count,1);
        assert_eq!((location.ranges[0].start_lba,location.ranges[0].end_lba,location.ranges[0].start_byte_offset,location.ranges[0].end_byte_offset),(0,0,512,1535));
        assert_eq!((location.ranges[1].start_lba,location.ranges[1].end_byte_offset),(2,2));
        let folder = Location::from_track_bytes(true,76,vec![(3000,76)],2048).unwrap();
        assert_eq!(folder.directory_storage,"catalog");
        assert!(Location::from_track_bytes(false,0,vec![],2048).unwrap().ranges.is_empty());
    }
    #[test]
    fn interleaved_ranges_and_partial_final_block() {
        let mut ranges = Vec::new();
        add_extent(&mut ranges, &Allocation { extent_loc: 100, extent_length: 5 * 2048 + 3,
            extended_attribute_record_length: 1, file_unit_size: 2, interleave_gap_size: 1 }, 2048).unwrap();
        assert_eq!(ranges.iter().map(|r| (r.start_lba, r.end_lba, r.end_byte_offset)).collect::<Vec<_>>(),
            [(101, 102, 2047), (104, 105, 2047), (107, 108, 2)]);
        add_extent(&mut ranges, &Allocation { extent_loc: 800, extent_length: 0,
            extended_attribute_record_length: 0, file_unit_size: 0, interleave_gap_size: 0 }, 2048).unwrap();
        assert_eq!(ranges.len(), 3);
    }
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("dx-entry-tools-{}-{}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            fs::create_dir(&path).unwrap(); Self(path)
        }
        fn file(&self, name: &str) -> String { self.0.join(name).to_string_lossy().into_owned() }
    }
    impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    fn both32(b: &mut [u8], at: usize, n: u32) { b[at..at+4].copy_from_slice(&n.to_le_bytes()); b[at+4..at+8].copy_from_slice(&n.to_be_bytes()); }
    fn record(name: &[u8], lba: u32, size: u32, flags: u8) -> Vec<u8> {
        let mut b = vec![0; (33 + name.len()).next_multiple_of(2)];
        b[0] = b.len() as u8; both32(&mut b, 2, lba); both32(&mut b, 10, size);
        b[18..25].copy_from_slice(&[126, 10, 20, 0, 0, 0, 0]); b[25] = flags;
        b[28] = 1; b[31] = 1; b[32] = name.len() as u8; b[33..33+name.len()].copy_from_slice(name); b
    }
    fn iso() -> Vec<u8> {
        let mut bytes = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/iso9660/test.iso")).unwrap();
        bytes.resize(64 * 2048, 0); both32(&mut bytes, 16 * 2048 + 80, 64);
        both32(&mut bytes, 16 * 2048 + 158, 24); both32(&mut bytes, 16 * 2048 + 166, 2048);
        bytes[24*2048..25*2048].fill(0); let mut pos = 24*2048;
        for rec in [record(&[0],24,2048,2), record(&[1],24,2048,2), record(b"PLAIN.BIN;1",32,2051,0),
            record(b"EMPTY;1",0,0,0), record(b"SUB",26,2048,2),
            record(b"SPLIT.BIN;1",30,2048,128), record(b"SPLIT.BIN;1",35,3,0)] {
            bytes[pos..pos+rec.len()].copy_from_slice(&rec); pos += rec.len();
        }
        bytes[32*2048..33*2048+3].fill(0x51); bytes
    }
    #[test]
    fn iso_locations_and_hashes_match_raw_and_cooked_bytes() {
        let temp = Temp::new(); let bytes = iso(); fs::write(temp.file("image.iso"), &bytes).unwrap();
        let expected = hash(&bytes[32*2048..33*2048+3]);
        for stride in [2352, 2448] {
            let mut raw = Vec::new();
            for block in bytes.chunks_exact(2048) {
                let pos = raw.len(); raw.resize(pos + stride, 0);
                raw[pos+1..pos+11].fill(0xff); raw[pos+15] = 1;
                raw[pos+16..pos+2064].copy_from_slice(block);
            }
            fs::write(temp.file(&format!("image{stride}.bin")), raw).unwrap();
            fs::write(temp.file(&format!("image{stride}.cue")), format!("FILE \"image{stride}.bin\" BINARY\n TRACK 01 MODE1/{stride}\n INDEX 01 00:00:00\n")).unwrap();
        }
        let track = DataTrack { bin_path: temp.0.join("image2448.bin"), track_offset: 0, user_data_offset: 16,
            stride: 2448, lba_offset: 0, descramble: false, form2_edc_reserved: false, sector_count: 64 };
        let fs = open_iso_fs(&track).unwrap();
        let loc = iso_location(&fs, "/PLAIN.BIN", NameSpace::Iso).unwrap();
        assert_eq!(loc.ranges[0].end_lba,33);
        let mut progress = |_| {};
        let mut writer = HashWriter::new(cancel(), &mut progress);
        copy_file_from_fs(&fs,"/PLAIN.BIN",&mut writer,NameSpace::Iso).unwrap();
        assert_eq!(writer.finish().unwrap().sha256,expected.sha256);
        for name in ["image.iso", "image2352.cue"] {
            let image = temp.file(name); let path = "/PLAIN.BIN".to_string(); let mut location = None;
            dispatch_single_file(image.clone(), path.clone(), Some("ISO 9660".into()), &mut Output::Location(&mut location)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let loc = location.unwrap(); assert_eq!(loc.ranges.len(), 1);
            assert_eq!((loc.ranges[0].start_lba, loc.ranges[0].end_lba, loc.ranges[0].end_byte_offset), (32,33,2));
            let result = hash_file(image.clone(), path.clone(), Some("ISO 9660".into()), 0, cancel(), &mut |_| {}).unwrap();
            assert_eq!(result.sha256, expected.sha256, "{name}"); let dest = temp.file("extracted.bin");
            extract_single_file(image.clone(), path, dest.clone(), Some("ISO 9660".into())).unwrap();
            assert_eq!(fs::read(dest).unwrap(), bytes[32*2048..33*2048+3], "{name}");
            assert!(hash_file(image.clone(), "/SUB".into(), Some("ISO 9660".into()), 0, cancel(), &mut |_| {}).unwrap_err().contains("files only"));
            assert!(hash_file(image.clone(), "/SPLIT.BIN".into(), Some("ISO 9660".into()), 0, cancel(), &mut |_| {}).unwrap_err().contains("allocation layout"));
            let mut location = None;
            dispatch_single_file(image.clone(), "/SPLIT.BIN".into(), Some("ISO 9660".into()), &mut Output::Location(&mut location)).unwrap();
            let loc = location.unwrap(); assert_eq!(loc.declared_bytes,2051);
            assert_eq!(loc.ranges.iter().map(|r| (r.start_lba,r.end_lba,r.end_byte_offset)).collect::<Vec<_>>(),[(30,30,2047),(35,35,2)]);
            let mut directory = None;
            dispatch_single_file(image, "/SUB".into(), Some("ISO 9660".into()), &mut Output::Location(&mut directory)).unwrap();
            assert!(directory.as_ref().unwrap().is_directory); assert_eq!(directory.unwrap().ranges[0].start_lba,26);
        }
    }
    #[test]
    fn xa_hashes_follow_content_subheader_and_raw_modes() {
        let temp = Temp::new(); let mut bytes = iso();
        // Change PLAIN.BIN's declared length to two full logical sectors.
        let record = 24 * 2048 + record(&[0],24,2048,2).len() + record(&[1],24,2048,2).len();
        both32(&mut bytes,record+10,4096);
        let mut raw = Vec::new();
        for (i, block) in bytes.chunks_exact(2048).enumerate() {
            let pos = raw.len(); raw.resize(pos+2352,0);
            raw[pos+1..pos+11].fill(0xff); raw[pos+15] = 2;
            let submode = if i == 33 { 0x68 } else if i == 32 { 0x48 } else { 0x08 };
            raw[pos+18] = submode; raw[pos+22] = submode;
            raw[pos+24..pos+2072].copy_from_slice(block);
            if i == 33 { raw[pos+2072..pos+2348].fill(0xab); }
        }
        fs::write(temp.file("xa.bin"),&raw).unwrap();
        fs::write(temp.file("xa.cue"),"FILE \"xa.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n").unwrap();
        let mut content = raw[32*2352+24..32*2352+2072].to_vec();
        content.extend_from_slice(&raw[33*2352+24..33*2352+2348]);
        let mut subheader = raw[32*2352+16..33*2352].to_vec();
        subheader.extend_from_slice(&raw[33*2352+16..34*2352]);
        for (mode, expected) in [(0,content),(1,subheader),(2,raw[32*2352..34*2352].to_vec())] {
            let result = hash_file(temp.file("xa.cue"),"/PLAIN.BIN".into(),Some("ISO 9660".into()),mode,cancel(),&mut |_|{}).unwrap();
            assert_eq!(result.bytes,expected.len() as u64,"mode {mode}");
            assert_eq!(result.sha256,hash(&expected).sha256,"mode {mode}");
        }
        assert_eq!(xa_mode(),XA_CONTENT);
        raw.truncate(33*2352+34);
        fs::write(temp.file("short-xa.bin"),raw).unwrap();
        fs::write(temp.file("short-xa.cue"),"FILE \"short-xa.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n").unwrap();
        for mode in [0,1,2] {
            assert!(hash_file(temp.file("short-xa.cue"),"/PLAIN.BIN".into(),Some("ISO 9660".into()),mode,cancel(),&mut |_|{}).is_err());
        }
    }
    #[test]
    fn truncated_iso_does_not_return_a_checksum() {
        let temp = Temp::new(); let mut bytes = iso();
        bytes.truncate(33*2048+1);
        fs::write(temp.file("short.iso"),bytes).unwrap();
        let result = hash_file(temp.file("short.iso"),"/PLAIN.BIN".into(),Some("ISO 9660".into()),0,cancel(),&mut |_|{});
        assert!(result.unwrap_err().contains("Incomplete ISO sector"));
    }
    #[test]
    fn host_hashes_and_disk_writer_failure_and_empty_files() {
        let temp = Temp::new(); fs::write(temp.file("file"), b"123456789").unwrap();
        let result = hash_file(temp.0.to_string_lossy().into_owned(), "/file".into(), None,0,cancel(),&mut |_|{}).unwrap();
        assert_eq!(result.sha256,hash(b"123456789").sha256);
        let path = temp.file("out"); fs::write(&path,b"keep").unwrap();
        assert!(write_to_path(&path, |_|Err("lookup failed".into())).is_err()); assert_eq!(fs::read(&path).unwrap(),b"keep");
        write_to_path(&path, |_|Ok(())).unwrap(); assert_eq!(fs::metadata(&path).unwrap().len(),0);
    }
}

#[cfg(test)]
mod real_tests {
    use super::*;
    #[test]
    #[ignore = "requires an HFS CUE via DX_IMG"]
    fn hfs_real_allocation_matches_track_bytes() {
        let image = std::env::var("DX_IMG").unwrap();
        let track = parse_track(&image).unwrap();
        let mut stack = vec!["/".to_string()]; let mut checked = 0;
        for _ in 0..80 {
            let Some(dir) = stack.pop() else { break; };
            let mut directory = None;
            dispatch_single_file(image.clone(),dir.clone(),Some("HFS".into()),&mut Output::Location(&mut directory)).unwrap();
            let directory = directory.unwrap();
            assert!(directory.is_directory); assert_eq!(directory.directory_storage,"catalog");
            assert!(!directory.ranges.is_empty());
            let list = list_disc_contents(image.clone(),dir.clone(),Some("HFS".into()),false).unwrap();
            for e in list {
                let path = if dir == "/" { format!("/{}",e.name) } else { format!("{dir}/{}",e.name) };
                if e.is_dir { stack.push(path); continue; }
                if e.size_bytes > 2_000_000 { continue; }
                let mut location = None;
                dispatch_single_file(image.clone(),path.clone(),Some("HFS".into()),&mut Output::Location(&mut location)).unwrap();
                let location = location.unwrap(); assert!(!location.is_directory);
                let mut bytes = Vec::new(); let mut file = File::open(&track.bin_path).unwrap();
                for range in &location.ranges {
                    for lba in range.start_lba..=range.end_lba {
                        let from = if lba == range.start_lba { range.start_byte_offset as usize } else { 0 };
                        let to = if lba == range.end_lba { range.end_byte_offset as usize + 1 } else { 2048 };
                        let pos = track.track_offset + lba * track.stride + track.user_data_offset + from as u64;
                        file.seek(SeekFrom::Start(pos)).unwrap();
                        let mut part = vec![0;to-from]; file.read_exact(&mut part).unwrap(); bytes.extend(part);
                    }
                }
                assert_eq!(bytes.len() as u64,location.declared_bytes);
                let result = hash_file(image.clone(),path.clone(),Some("HFS".into()),0,Arc::new(AtomicBool::new(false)),&mut |_|{}).unwrap();
                assert_eq!(result.sha256,format!("{:x}",Sha256::digest(&bytes)),"{path}");
                println!("{path}: {} bytes, {} ranges",bytes.len(),location.ranges.len());
                checked += 1;
                if checked >= 8 { return; }
            }
        }
        assert!(checked > 0);
    }
    #[test]
    #[ignore = "requires DX_IMG"]
    fn real_image_streams_match_extracted_files() {
        let image = std::env::var("DX_IMG").unwrap();
        let filesystems = get_disc_filesystems(image.clone()).unwrap();
        let mut checked = 0;
        for filesystem in filesystems {
            if matches!(filesystem.as_str(), "Path Table" | "El Torito") { continue; }
            let mut stack = vec!["/".to_string()]; let mut dirs = 0; let mut count = 0;
            while let Some(dir) = stack.pop() {
                dirs += 1; if dirs > 80 || count >= 2 { break; }
                let entries = list_disc_contents(image.clone(), dir.clone(), Some(filesystem.clone()), false).unwrap();
                for e in entries {
                    let path = if dir == "/" { format!("/{}",e.name) } else { format!("{dir}/{}",e.name) };
                    if e.is_dir { if !e.deleted { stack.push(path); } continue; }
                    if e.size_bytes == 0 || e.size_bytes > 2_000_000 || e.deleted { continue; }
                    let dest = std::env::temp_dir().join(format!("dx-hash-real-{}-{}.bin",std::process::id(),checked));
                    for mode in if e.is_xa { vec![0,1,2] } else { vec![0] } {
                        XA_MODE.with(|c| c.set(mode));
                        let extraction = extract_single_file(image.clone(),path.clone(),dest.to_string_lossy().into_owned(),Some(filesystem.clone()));
                        XA_MODE.with(|c| c.set(0)); extraction.unwrap();
                        let bytes = fs::read(&dest).unwrap(); let _ = fs::remove_file(&dest);
                        let result = hash_file(image.clone(),path.clone(),Some(filesystem.clone()),mode,Arc::new(AtomicBool::new(false)),&mut |_|{}).unwrap();
                        assert_eq!(result.bytes,bytes.len() as u64);
                        assert_eq!(result.sha256,format!("{:x}",Sha256::digest(&bytes)));
                        println!("{filesystem} {path}: {} bytes, mode {mode}",result.bytes);
                    }
                    checked += 1; count += 1; if count >= 2 { break; }
                }
            }
        }
        assert!(checked > 0,"no suitable files found in {image}");
    }
}
