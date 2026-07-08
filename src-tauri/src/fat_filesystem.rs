// FAT12/FAT16 filesystem reader for floppy (and small hard-disk) images.
//
// Built for the KryoFlux flow (decoded flux → flat 512-byte-sector image) but
// works on any flat FAT12/16 volume. Supports long file names (VFAT LFN) and
// deleted-entry recovery: an erased file keeps its directory record with 0xE5
// over the first name byte, and its data is recovered with the standard
// contiguous-cluster undelete heuristic (the FAT chain itself is freed).

use crate::DiscEntry;
use std::io::{Read, Seek, SeekFrom, Write};
use std::fs::File;

const DIRENT_SIZE: usize = 32;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LFN: u8 = 0x0F;
const DELETED: u8 = 0xE5;

#[derive(Clone)]
struct RawEntry {
    name: String,
    is_dir: bool,
    first_cluster: u32,
    size: u32,
    modified: String,
    deleted: bool,
}

pub struct FatFs<F: Read + Seek> {
    file: F,
    bytes_per_sector: u64,
    sectors_per_cluster: u64,
    root_start: u64,   // byte offset of the fixed root directory
    root_bytes: u64,
    data_start: u64,   // byte offset of cluster 2
    fat: Vec<u8>,      // first FAT, raw
    fat12: bool,
    total_clusters: u32,
    pub label: String, // "FAT12" / "FAT16"
}

// Quick validation used by format dispatchers.
pub fn is_fat_boot_sector(boot: &[u8]) -> bool {
    if boot.len() < 512 {
        return false;
    }
    let bps = u16::from_le_bytes([boot[11], boot[12]]);
    let spc = boot[13];
    let nfats = boot[16];
    let root_entries = u16::from_le_bytes([boot[17], boot[18]]);
    let total16 = u16::from_le_bytes([boot[19], boot[20]]);
    let media = boot[21];
    let fat_size = u16::from_le_bytes([boot[22], boot[23]]);
    matches!(bps, 512 | 1024 | 2048 | 4096)
        && spc.is_power_of_two()
        && (1..=2).contains(&nfats)
        && root_entries > 0
        && total16 > 0
        && fat_size > 0
        && (media == 0xF0 || media >= 0xF8)
}

impl<F: Read + Seek> FatFs<F> {
    pub fn new(mut file: F) -> Result<Self, String> {
        let mut boot = [0u8; 512];
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        file.read_exact(&mut boot).map_err(|e| format!("Cannot read boot sector: {e}"))?;
        if !is_fat_boot_sector(&boot) {
            return Err("Not a FAT12/16 volume".to_string());
        }
        let bps = u16::from_le_bytes([boot[11], boot[12]]) as u64;
        let spc = boot[13] as u64;
        let reserved = u16::from_le_bytes([boot[14], boot[15]]) as u64;
        let nfats = boot[16] as u64;
        let root_entries = u16::from_le_bytes([boot[17], boot[18]]) as u64;
        let total_sectors = u16::from_le_bytes([boot[19], boot[20]]) as u64;
        let fat_size = u16::from_le_bytes([boot[22], boot[23]]) as u64;

        let fat_start = reserved * bps;
        let root_start = fat_start + nfats * fat_size * bps;
        let root_bytes = root_entries * DIRENT_SIZE as u64;
        // Root directory occupies whole sectors.
        let root_sectors = root_bytes.div_ceil(bps);
        let data_start = root_start + root_sectors * bps;
        let data_sectors = total_sectors.saturating_sub(data_start / bps);
        let total_clusters = (data_sectors / spc) as u32;

        let mut fat = vec![0u8; (fat_size * bps) as usize];
        file.seek(SeekFrom::Start(fat_start)).map_err(|e| e.to_string())?;
        file.read_exact(&mut fat).map_err(|e| format!("Cannot read FAT: {e}"))?;

        // The FAT12/16 boundary is defined by cluster count, not by the label.
        let fat12 = total_clusters < 4085;
        Ok(FatFs {
            file,
            bytes_per_sector: bps,
            sectors_per_cluster: spc,
            root_start,
            root_bytes,
            data_start,
            fat,
            fat12,
            total_clusters,
            label: if fat12 { "FAT12".to_string() } else { "FAT16".to_string() },
        })
    }

    fn bytes_per_cluster(&self) -> u64 {
        self.bytes_per_sector * self.sectors_per_cluster
    }

    fn fat_entry(&self, cl: u32) -> u32 {
        if self.fat12 {
            let off = cl as usize * 3 / 2;
            if off + 1 >= self.fat.len() {
                return 0xFFF;
            }
            let v = u16::from_le_bytes([self.fat[off], self.fat[off + 1]]);
            if cl & 1 == 0 { (v & 0x0FFF) as u32 } else { (v >> 4) as u32 }
        } else {
            let off = cl as usize * 2;
            if off + 1 >= self.fat.len() {
                return 0xFFFF;
            }
            u16::from_le_bytes([self.fat[off], self.fat[off + 1]]) as u32
        }
    }

    fn end_of_chain(&self, v: u32) -> bool {
        if self.fat12 { v >= 0xFF8 } else { v >= 0xFFF8 }
    }

    fn cluster_chain(&self, first: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut cur = first;
        let max = self.total_clusters + 2;
        while (2..max).contains(&cur) && chain.len() < max as usize {
            chain.push(cur);
            let next = self.fat_entry(cur);
            if self.end_of_chain(next) || next < 2 {
                break;
            }
            cur = next;
        }
        chain
    }

    fn read_cluster(&mut self, cl: u32) -> Result<Vec<u8>, String> {
        let off = self.data_start + (cl as u64 - 2) * self.bytes_per_cluster();
        let mut buf = vec![0u8; self.bytes_per_cluster() as usize];
        self.file.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        // Tolerate a short read at the end of a truncated image.
        let mut filled = 0;
        while filled < buf.len() {
            match self.file.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(buf)
    }

    // Raw bytes of a directory: the fixed root region, or a cluster chain.
    fn dir_bytes(&mut self, first_cluster: u32) -> Result<Vec<u8>, String> {
        if first_cluster == 0 {
            let mut buf = vec![0u8; self.root_bytes as usize];
            self.file.seek(SeekFrom::Start(self.root_start)).map_err(|e| e.to_string())?;
            self.file.read_exact(&mut buf).map_err(|e| format!("Cannot read root dir: {e}"))?;
            return Ok(buf);
        }
        let chain = if self.cluster_chain(first_cluster).is_empty() {
            // Deleted directory: its chain is freed — read the first cluster.
            vec![first_cluster]
        } else {
            self.cluster_chain(first_cluster)
        };
        let mut out = Vec::new();
        for cl in chain {
            out.extend_from_slice(&self.read_cluster(cl)?);
        }
        Ok(out)
    }

    fn parse_dir(&mut self, first_cluster: u32) -> Result<Vec<RawEntry>, String> {
        let data = self.dir_bytes(first_cluster)?;
        let mut out = Vec::new();
        // Pending VFAT long-name fragments (stored before the 8.3 entry, last
        // fragment first).
        let mut lfn_parts: Vec<String> = Vec::new();
        for chunk in data.chunks_exact(DIRENT_SIZE) {
            let first = chunk[0];
            if first == 0x00 {
                break; // end of directory
            }
            let attrs = chunk[11];
            if attrs == ATTR_LFN {
                // 13 UCS-2 chars at 1..11, 14..26, 28..32.
                let mut chars: Vec<u16> = Vec::with_capacity(13);
                for r in [(1usize, 11usize), (14, 26), (28, 32)] {
                    for o in (r.0..r.1).step_by(2) {
                        chars.push(u16::from_le_bytes([chunk[o], chunk[o + 1]]));
                    }
                }
                while matches!(chars.last(), Some(0x0000) | Some(0xFFFF)) {
                    chars.pop();
                }
                lfn_parts.push(String::from_utf16_lossy(&chars));
                continue;
            }
            if first == DELETED && chunk[1..].iter().all(|&b| b == 0) {
                // Fully wiped slot — nothing recoverable.
                lfn_parts.clear();
                continue;
            }
            if attrs & ATTR_VOLUME_ID != 0 {
                lfn_parts.clear();
                continue;
            }
            let deleted = first == DELETED;
            // 8.3 short name; 0x05 escapes a real leading 0xE5. For deleted
            // entries the first character is lost — substitute '_'.
            let lead = match first {
                0x05 => 0xE5u8,
                DELETED => b'_',
                b => b,
            };
            let mut base: Vec<u8> = vec![lead];
            base.extend_from_slice(&chunk[1..8]);
            let base_str = String::from_utf8_lossy(&base).trim_end().to_string();
            let ext_str = String::from_utf8_lossy(&chunk[8..11]).trim_end().to_string();
            let mut short = if ext_str.is_empty() { base_str.clone() } else { format!("{base_str}.{ext_str}") };
            if short.is_empty() || short == "." || short == ".." {
                lfn_parts.clear();
                continue;
            }
            // Prefer the long name when fragments are present (they precede
            // the short entry in reverse order).
            if !lfn_parts.is_empty() {
                lfn_parts.reverse();
                let long: String = lfn_parts.concat();
                lfn_parts.clear();
                if !long.is_empty() {
                    short = long;
                }
            }
            // Skip implausible deleted remains (reused/garbage slots).
            if deleted && !short.chars().any(|c| c.is_ascii_alphanumeric()) {
                continue;
            }
            let is_dir = attrs & ATTR_DIRECTORY != 0;
            let cluster = u16::from_le_bytes([chunk[26], chunk[27]]) as u32;
            let size = u32::from_le_bytes([chunk[28], chunk[29], chunk[30], chunk[31]]);
            out.push(RawEntry {
                name: short,
                is_dir,
                first_cluster: cluster,
                size,
                modified: fat_datetime(
                    u16::from_le_bytes([chunk[24], chunk[25]]),
                    u16::from_le_bytes([chunk[22], chunk[23]]),
                ),
                deleted,
            });
        }
        Ok(out)
    }

    fn resolve(&mut self, path: &str) -> Result<RawEntry, String> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut entry = RawEntry {
            name: String::new(),
            is_dir: true,
            first_cluster: 0, // 0 = fixed root region
            size: 0,
            modified: String::new(),
            deleted: false,
        };
        for comp in parts {
            if !entry.is_dir {
                return Err(format!("Not a directory: {comp}"));
            }
            let listing = self.parse_dir(entry.first_cluster)?;
            // Prefer a live entry; fall back to a deleted one so recovered
            // files stay reachable for extraction.
            entry = listing
                .iter()
                .find(|e| !e.deleted && e.name.eq_ignore_ascii_case(comp))
                .cloned()
                .or_else(|| listing.into_iter().find(|e| e.name.eq_ignore_ascii_case(comp)))
                .ok_or_else(|| format!("Path not found: {comp}"))?;
        }
        Ok(entry)
    }

    pub fn list_directory(&mut self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let dir = self.resolve(dir_path.trim_matches('/'))?;
        if !dir.is_dir {
            return Err(format!("Not a directory: {dir_path}"));
        }
        Ok(self
            .parse_dir(dir.first_cluster)?
            .into_iter()
            .map(|e| DiscEntry {
                name: e.name,
                is_dir: e.is_dir,
                lba: e.first_cluster,
                size: if e.is_dir { 0 } else { e.size },
                size_bytes: e.size,
                modified: e.modified,
                deleted: e.deleted,
            })
            .collect())
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let entry = self.resolve(file_path.trim_matches('/'))?;
        if entry.is_dir {
            return Err(format!("Not a file: {file_path}"));
        }
        let chain = if entry.deleted {
            // Deletion freed the FAT chain; assume the file was contiguous
            // from its first cluster (the standard undelete heuristic).
            let n = (entry.size as u64).div_ceil(self.bytes_per_cluster()).max(1) as u32;
            (0..n)
                .map(|i| entry.first_cluster.saturating_add(i))
                .filter(|&c| (2..self.total_clusters + 2).contains(&c))
                .collect()
        } else {
            self.cluster_chain(entry.first_cluster)
        };
        let mut out = File::create(dest_path).map_err(|e| format!("Cannot create output: {e}"))?;
        let mut remaining = entry.size as u64;
        for cl in chain {
            if remaining == 0 {
                break;
            }
            let data = self.read_cluster(cl)?;
            let n = remaining.min(self.bytes_per_cluster()) as usize;
            out.write_all(&data[..n.min(data.len())]).map_err(|e| format!("Write error: {e}"))?;
            remaining = remaining.saturating_sub(self.bytes_per_cluster());
        }
        Ok(())
    }
}

// Decode packed FAT date/time to "YYYY-MM-DD HH:MM:SS", or "" if unset.
fn fat_datetime(date: u16, time: u16) -> String {
    if date == 0 {
        return String::new();
    }
    let year = 1980 + (date >> 9) as u32;
    let month = ((date >> 5) & 0xF) as u32;
    let day = (date & 0x1F) as u32;
    let hour = (time >> 11) as u32;
    let min = ((time >> 5) & 0x3F) as u32;
    let sec = ((time & 0x1F) * 2) as u32;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 {
        return String::new();
    }
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}")
}
