// FATX / XTAF filesystem reader (Xbox and Xbox 360 hard-drive / dev-drive images).
//
// Ported from aerosoul94/FATXTools (FATX/FileSystem/Volume.cs,
// DirectoryEntry.cs, Constants.cs, TimeStamp.cs).
//
// FATX is the FAT-derived filesystem used on Xbox and Xbox 360 storage. The
// two consoles use opposite byte orders, distinguished by the volume signature
// at the start of each partition:
//   "FATX" (46 41 54 58)  → original Xbox, little-endian
//   "XTAF" (58 54 46 41)  → Xbox 360,      big-endian
//
// Volume header (non-legacy, fields in the partition's byte order):
//   0x00  u32  signature (reads as 0x58544146 in the native order)
//   0x04  u32  serial number
//   0x08  u32  sectors per cluster
//   0x0C  u32  root directory first cluster
//
// Layout within a partition (all sizes derived, never stored on disk):
//   bytes 0..0x1000        : reserved (header lives here)
//   bytes 0x1000..fileArea : file allocation table (FAT16 or FAT32), page-padded
//   bytes fileArea..end    : cluster file area; cluster N at (N-1)*bytesPerCluster
//
// Directory entry (0x40 bytes):
//   0x00  u8        file name length (0x00/0xFF = end of dir, 0xE5 = deleted)
//   0x01  u8        attributes (0x10 = directory)
//   0x02  [42]u8    file name (ASCII)
//   0x2C  u32       first cluster
//   0x30  u32       file size in bytes
//   0x34  u32       creation time   (packed)
//   0x38  u32       last write time (packed)
//   0x3C  u32       last access time(packed)
//
// A full HDD image carries several FATX partitions at fixed offsets and has no
// on-disk partition table. A single-partition dump simply begins with the
// volume header at offset 0. Both cases are handled: offset 0 is probed first,
// then the standard original-Xbox partition map. Every candidate is gated on a
// valid signature, so probing offsets that don't apply is harmless.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use crate::DiscEntry;

const SECTOR_SIZE: u64 = 0x200;
const PAGE_SIZE: u64 = 0x1000;
const RESERVED_BYTES: u64 = PAGE_SIZE;
const RESERVED_CLUSTERS: u64 = 1;
const DIRENT_SIZE: u64 = 0x40;

const DIRENT_END_A: u8 = 0x00;
const DIRENT_END_B: u8 = 0xFF;
const DIRENT_DELETED: u8 = 0xE5;
const ATTR_DIRECTORY: u8 = 0x10;

const MAGIC_FATX: &[u8; 4] = b"FATX"; // original Xbox, little-endian
const MAGIC_XTAF: &[u8; 4] = b"XTAF"; // Xbox 360, big-endian

const FAT16_RESERVED: u32 = 0xFFF0;
const FAT32_RESERVED: u32 = 0xFFFF_FFF0;

// ── byte helpers ────────────────────────────────────────────────────────────

fn rd_u32(buf: &[u8], off: usize, be: bool) -> u32 {
    let b = [buf[off], buf[off + 1], buf[off + 2], buf[off + 3]];
    if be { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
}

fn rd_u16(buf: &[u8], off: usize, be: bool) -> u16 {
    let b = [buf[off], buf[off + 1]];
    if be { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
}

// ── partition geometry ──────────────────────────────────────────────────────

#[derive(Clone)]
struct Partition {
    name: String,
    offset: u64, // absolute byte offset of the volume header
    big_endian: bool,
    root_dir_first_cluster: u32,
    bytes_per_cluster: u64,
    max_clusters: u32,
    is_fat16: bool,
    file_area_byte_offset: u64,
}

impl Partition {
    fn cluster_to_physical(&self, cluster: u32) -> u64 {
        self.offset + self.file_area_byte_offset
            + self.bytes_per_cluster * (cluster as u64 - 1)
    }
}

// Parse a volume header at `offset`. Returns None if the signature or geometry
// is not a plausible FATX volume.
fn parse_partition<F: Read + Seek>(reader: &mut F, name: &str, offset: u64, length: u64) -> Option<Partition> {
    if length < PAGE_SIZE { return None; }
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut hdr = [0u8; 16];
    reader.read_exact(&mut hdr).ok()?;

    let big_endian = if &hdr[0..4] == MAGIC_FATX {
        false
    } else if &hdr[0..4] == MAGIC_XTAF {
        true
    } else {
        return None;
    };

    let sectors_per_cluster = rd_u32(&hdr, 8, big_endian);
    let root_dir_first_cluster = rd_u32(&hdr, 12, big_endian);

    // Sanity-check geometry to reject false positives. Real FATX clusters are a
    // power-of-two number of 512-byte sectors and the root is always cluster 1+.
    if sectors_per_cluster == 0
        || sectors_per_cluster > 0x400
        || !sectors_per_cluster.is_power_of_two()
        || root_dir_first_cluster == 0
    {
        return None;
    }

    let bytes_per_cluster = sectors_per_cluster as u64 * SECTOR_SIZE;
    let max_clusters = length / bytes_per_cluster + RESERVED_CLUSTERS;
    if max_clusters > u32::MAX as u64 { return None; }

    let (is_fat16, raw_fat_bytes) = if max_clusters < FAT16_RESERVED as u64 {
        (true, max_clusters * 2)
    } else {
        (false, max_clusters * 4)
    };
    let bytes_per_fat = (raw_fat_bytes + (PAGE_SIZE - 1)) & !(PAGE_SIZE - 1);
    let file_area_byte_offset = RESERVED_BYTES + bytes_per_fat;
    if file_area_byte_offset >= length { return None; }
    if root_dir_first_cluster as u64 >= max_clusters { return None; }

    Some(Partition {
        name: name.to_string(),
        offset,
        big_endian,
        root_dir_first_cluster,
        bytes_per_cluster,
        max_clusters: max_clusters as u32,
        is_fat16,
        file_area_byte_offset,
    })
}

// Candidate partitions for a given image length. The original Xbox HDD has a
// fixed, table-less partition map; offset 0 covers single-partition dumps for
// both consoles. Each candidate is validated against the on-disk signature.
fn candidate_partitions(len: u64) -> Vec<(&'static str, u64, u64)> {
    // (name, offset, length). Original-Xbox fixed map in bytes.
    const X: u64 = 0x0000_0000;
    let xbox_map: [(&str, u64, u64); 5] = [
        ("Partition3 (Cache X)", 0x0008_0000, 0x2EE0_0000),
        ("Partition4 (Cache Y)", 0x2EE8_0000, 0x2EE0_0000),
        ("Partition5 (Cache Z)", 0x5DC8_0000, 0x2EE0_0000),
        ("Partition2 (System C)", 0x8CA8_0000, 0x1F40_0000),
        ("Partition1 (Data E)", 0xABE8_0000, 0),
    ];
    let _ = X;
    let mut out: Vec<(&'static str, u64, u64)> = Vec::new();
    // Single-partition dump: header at the very start of the image.
    out.push(("Partition", 0, len));
    for (name, off, plen) in xbox_map {
        if off >= len { continue; }
        let length = if plen == 0 || off + plen > len { len - off } else { plen };
        out.push((name, off, length));
    }
    out
}

fn discover<F: Read + Seek>(reader: &mut F) -> Vec<Partition> {
    let len = reader.seek(SeekFrom::End(0)).unwrap_or(0);
    if len == 0 { return Vec::new(); }
    let mut parts = Vec::new();
    let mut seen_offsets = HashSet::new();
    for (name, off, plen) in candidate_partitions(len) {
        if !seen_offsets.insert(off) { continue; }
        if let Some(p) = parse_partition(reader, name, off, plen) {
            parts.push(p);
        }
    }
    parts
}

// ── detection ───────────────────────────────────────────────────────────────

pub fn is_fatx_image(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    !discover(&mut f).is_empty()
}

// ── directory entries ─────────────────────────────────────────────────────────

struct RawEntry {
    name: String,
    is_dir: bool,
    first_cluster: u32,
    size: u32,
    modified: String,
}

// Decode a packed FATX timestamp into "YYYY-MM-DD HH:MM:SS", or "" if invalid.
fn format_time(t: u32, is_x360: bool) -> String {
    if t == 0 { return String::new(); }
    let year = ((t & 0xFE00_0000) >> 25) + if is_x360 { 1980 } else { 2000 };
    let month = (t & 0x01E0_0000) >> 21;
    let day = (t & 0x001F_0000) >> 16;
    let hour = (t & 0x0000_F800) >> 11;
    let minute = (t & 0x0000_07E0) >> 5;
    let second = (t & 0x0000_001F) * 2;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day)
        || hour > 23 || minute > 59 || second > 59
    {
        return String::new();
    }
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

// ── the filesystem ────────────────────────────────────────────────────────────

pub struct FatxFs<F: Read + Seek> {
    file: F,
    partitions: Vec<Partition>,
}

impl<F: Read + Seek> FatxFs<F> {
    pub fn new(mut file: F) -> Result<Self, String> {
        let partitions = discover(&mut file);
        if partitions.is_empty() {
            return Err("No FATX/XTAF partitions found".to_string());
        }
        Ok(FatxFs { file, partitions })
    }

    fn multi(&self) -> bool {
        self.partitions.len() > 1
    }

    // Map a virtual path to (partition index, path within that partition).
    fn split_partition(&self, path: &str) -> Result<(usize, String), String> {
        let p = path.trim_matches('/');
        if !self.multi() {
            return Ok((0, p.to_string()));
        }
        let mut it = p.splitn(2, '/');
        let first = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("");
        let idx = self
            .partitions
            .iter()
            .position(|pt| pt.name.eq_ignore_ascii_case(first))
            .ok_or_else(|| format!("Unknown partition: {first}"))?;
        Ok((idx, rest.to_string()))
    }

    fn read_fat(&mut self, part: &Partition) -> Result<Vec<u32>, String> {
        let entry_size = if part.is_fat16 { 2 } else { 4 };
        let n = part.max_clusters as usize;
        let mut raw = vec![0u8; n * entry_size];
        self.file
            .seek(SeekFrom::Start(part.offset + RESERVED_BYTES))
            .map_err(|e| format!("Seek error: {e}"))?;
        self.file
            .read_exact(&mut raw)
            .map_err(|e| format!("FAT read error: {e}"))?;
        let mut fat = vec![0u32; n];
        for i in 0..n {
            fat[i] = if part.is_fat16 {
                rd_u16(&raw, i * 2, part.big_endian) as u32
            } else {
                rd_u32(&raw, i * 4, part.big_endian)
            };
        }
        Ok(fat)
    }

    fn cluster_chain(&self, part: &Partition, fat: &[u32], first: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        if first == 0 || first as usize >= fat.len() {
            return chain;
        }
        chain.push(first);
        let reserved = if part.is_fat16 { FAT16_RESERVED } else { FAT32_RESERVED };
        let mut visited = HashSet::new();
        visited.insert(first);
        let mut cur = first;
        loop {
            let next = fat[cur as usize];
            if next >= reserved {
                break;
            }
            if next == 0 || next as usize >= fat.len() || !visited.insert(next) {
                break; // free, out of range, or cyclic — stop here
            }
            chain.push(next);
            cur = next;
        }
        chain
    }

    fn read_cluster(&mut self, part: &Partition, cluster: u32) -> Result<Vec<u8>, String> {
        let offset = part.cluster_to_physical(cluster);
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Seek error: {e}"))?;
        let mut buf = vec![0u8; part.bytes_per_cluster as usize];
        // Tolerate a short final cluster on truncated images by zero-filling.
        let mut filled = 0usize;
        while filled < buf.len() {
            match self.file.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(format!("Read error: {e}")),
            }
        }
        Ok(buf)
    }

    // Read every directory entry across a directory's cluster chain.
    fn read_dir(&mut self, part: &Partition, fat: &[u32], first_cluster: u32) -> Result<Vec<RawEntry>, String> {
        let chain = self.cluster_chain(part, fat, first_cluster);
        let per_cluster = (part.bytes_per_cluster / DIRENT_SIZE) as usize;
        let mut out = Vec::new();
        for cl in chain {
            let data = self.read_cluster(part, cl)?;
            let mut hit_end = false;
            for i in 0..per_cluster {
                let off = i * DIRENT_SIZE as usize;
                if off + DIRENT_SIZE as usize > data.len() {
                    break;
                }
                let name_len = data[off];
                if name_len == DIRENT_END_A || name_len == DIRENT_END_B {
                    hit_end = true;
                    break;
                }
                let attrs = data[off + 1];
                let is_dir = attrs & ATTR_DIRECTORY != 0;
                let first = rd_u32(&data, off + 0x2C, part.big_endian);
                let size = rd_u32(&data, off + 0x30, part.big_endian);
                let write_time = rd_u32(&data, off + 0x38, part.big_endian);

                if name_len == DIRENT_DELETED {
                    continue; // skip deleted entries
                }
                let real_len = (name_len as usize).min(42);
                let name = String::from_utf8_lossy(&data[off + 2..off + 2 + real_len]).into_owned();
                out.push(RawEntry {
                    name,
                    is_dir,
                    first_cluster: first,
                    size,
                    modified: format_time(write_time, part.big_endian),
                });
            }
            if hit_end {
                break;
            }
        }
        Ok(out)
    }

    // Resolve a path within a partition to its directory entry. An empty path
    // resolves to the synthetic root (returned as a directory at the root cluster).
    fn resolve(&mut self, part: &Partition, fat: &[u32], path: &str) -> Result<RawEntry, String> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut entry = RawEntry {
            name: String::new(),
            is_dir: true,
            first_cluster: part.root_dir_first_cluster,
            size: 0,
            modified: String::new(),
        };
        for comp in parts {
            if !entry.is_dir {
                return Err(format!("Not a directory: {comp}"));
            }
            let listing = self.read_dir(part, fat, entry.first_cluster)?;
            entry = listing
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(comp))
                .ok_or_else(|| format!("Path not found: {comp}"))?;
        }
        Ok(entry)
    }

    pub fn list_directory(&mut self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let p = dir_path.trim_matches('/');
        // Multi-partition root: list the partitions as top-level directories.
        if self.multi() && p.is_empty() {
            return Ok(self
                .partitions
                .iter()
                .map(|pt| DiscEntry {
                    name: pt.name.clone(),
                    is_dir: true,
                    lba: 0,
                    size: 0,
                    size_bytes: 0,
                    modified: String::new(),
                })
                .collect());
        }

        let (idx, sub) = self.split_partition(p)?;
        let part = self.partitions[idx].clone();
        let fat = self.read_fat(&part)?;
        let dir = self.resolve(&part, &fat, &sub)?;
        if !dir.is_dir {
            return Err(format!("Not a directory: {dir_path}"));
        }
        let entries = self.read_dir(&part, &fat, dir.first_cluster)?;
        Ok(entries
            .into_iter()
            .map(|e| DiscEntry {
                name: e.name,
                is_dir: e.is_dir,
                lba: e.first_cluster,
                size: if e.is_dir { 0 } else { e.size },
                size_bytes: e.size,
                modified: e.modified,
            })
            .collect())
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let p = file_path.trim_matches('/');
        let (idx, sub) = self.split_partition(p)?;
        let part = self.partitions[idx].clone();
        let fat = self.read_fat(&part)?;
        let entry = self.resolve(&part, &fat, &sub)?;
        if entry.is_dir {
            return Err(format!("Not a file: {file_path}"));
        }
        let mut out = File::create(dest_path).map_err(|e| format!("Cannot create output: {e}"))?;
        self.write_file_data(&part, &fat, &entry, &mut out)
    }

    pub fn extract_directory(&mut self, dir_path: &str, dest_path: &str) -> Result<(), String> {
        std::fs::create_dir_all(dest_path).map_err(|e| format!("Cannot create directory: {e}"))?;
        let p = dir_path.trim_matches('/');

        // Multi-partition root: extract every partition into its own subfolder.
        if self.multi() && p.is_empty() {
            let names: Vec<String> = self.partitions.iter().map(|pt| pt.name.clone()).collect();
            for name in names {
                let child = Path::new(dest_path).join(&name);
                self.extract_directory(&name, child.to_string_lossy().as_ref())?;
            }
            return Ok(());
        }

        let (idx, sub) = self.split_partition(p)?;
        let part = self.partitions[idx].clone();
        let fat = self.read_fat(&part)?;
        let dir = self.resolve(&part, &fat, &sub)?;
        if !dir.is_dir {
            return Err(format!("Not a directory: {dir_path}"));
        }
        self.extract_dir_recursive(&part, &fat, dir.first_cluster, Path::new(dest_path))
    }

    fn extract_dir_recursive(&mut self, part: &Partition, fat: &[u32], cluster: u32, dest: &Path) -> Result<(), String> {
        for e in self.read_dir(part, fat, cluster)? {
            let child = dest.join(&e.name);
            if e.is_dir {
                std::fs::create_dir_all(&child)
                    .map_err(|err| format!("Cannot create {child:?}: {err}"))?;
                self.extract_dir_recursive(part, fat, e.first_cluster, &child)?;
            } else {
                let mut out = File::create(&child)
                    .map_err(|err| format!("Cannot create {child:?}: {err}"))?;
                self.write_file_data(part, fat, &e, &mut out)?;
            }
        }
        Ok(())
    }

    fn write_file_data(&mut self, part: &Partition, fat: &[u32], entry: &RawEntry, out: &mut File) -> Result<(), String> {
        let mut remaining = entry.size as u64;
        let chain = self.cluster_chain(part, fat, entry.first_cluster);
        for cl in chain {
            if remaining == 0 {
                break;
            }
            let data = self.read_cluster(part, cl)?;
            let n = remaining.min(part.bytes_per_cluster) as usize;
            out.write_all(&data[..n.min(data.len())])
                .map_err(|e| format!("Write error: {e}"))?;
            remaining = remaining.saturating_sub(part.bytes_per_cluster);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // Build a minimal single-partition little-endian FATX image with:
    //   root (cluster 1): HELLO.TXT -> cluster 2, "SUB" dir -> cluster 3
    //   SUB  (cluster 3): INNER.BIN -> cluster 4
    fn write_dirent(buf: &mut [u8], off: usize, name: &str, attrs: u8, first: u32, size: u32) {
        buf[off] = name.len() as u8;
        buf[off + 1] = attrs;
        buf[off + 2..off + 2 + name.len()].copy_from_slice(name.as_bytes());
        // pad rest of name field with 0xFF
        for b in &mut buf[off + 2 + name.len()..off + 0x2C] {
            *b = 0xFF;
        }
        buf[off + 0x2C..off + 0x30].copy_from_slice(&first.to_le_bytes());
        buf[off + 0x30..off + 0x34].copy_from_slice(&size.to_le_bytes());
    }

    fn build_image() -> Vec<u8> {
        let len = 0x8000usize;
        let mut img = vec![0u8; len];

        // Header: sectors_per_cluster = 1 -> bytes_per_cluster = 512.
        img[0..4].copy_from_slice(MAGIC_FATX);
        img[8..12].copy_from_slice(&1u32.to_le_bytes()); // sectors per cluster
        img[12..16].copy_from_slice(&1u32.to_le_bytes()); // root first cluster

        // Geometry mirrors parse_partition: bpc=512, max_clusters=65, fat16,
        // bytes_per_fat padded to 0x1000, file_area at 0x2000.
        let file_area = 0x2000usize;
        let bpc = 512usize;
        let cluster_off = |n: usize| file_area + bpc * (n - 1);

        // FAT16 at 0x1000: mark clusters 1..=4 as end-of-chain.
        for cl in 1..=4usize {
            let e = 0x1000 + cl * 2;
            img[e..e + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        }

        // Root directory (cluster 1).
        let root = cluster_off(1);
        write_dirent(&mut img, root, "HELLO.TXT", 0x20, 2, b"hello fatx".len() as u32);
        write_dirent(&mut img, root + 0x40, "SUB", ATTR_DIRECTORY, 3, 0);
        img[root + 0x80] = DIRENT_END_B; // end marker

        // HELLO.TXT data (cluster 2).
        let f = cluster_off(2);
        img[f..f + b"hello fatx".len()].copy_from_slice(b"hello fatx");

        // SUB directory (cluster 3).
        let sub = cluster_off(3);
        write_dirent(&mut img, sub, "INNER.BIN", 0x20, 4, 4);
        img[sub + 0x40] = DIRENT_END_B;

        // INNER.BIN data (cluster 4).
        let inner = cluster_off(4);
        img[inner..inner + 4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        img
    }

    #[test]
    fn discovers_single_partition() {
        let img = build_image();
        let mut fs = FatxFs::new(Cursor::new(img)).unwrap();
        assert_eq!(fs.partitions.len(), 1);
        assert!(!fs.multi());
        // Header endianness detected as little (original Xbox).
        assert!(!fs.partitions[0].big_endian);
    }

    #[test]
    fn lists_root_and_subdir() {
        let img = build_image();
        let mut fs = FatxFs::new(Cursor::new(img)).unwrap();

        let root = fs.list_directory("/").unwrap();
        assert_eq!(root.len(), 2);
        let hello = root.iter().find(|e| e.name == "HELLO.TXT").unwrap();
        assert!(!hello.is_dir);
        assert_eq!(hello.size_bytes, 10);
        let sub = root.iter().find(|e| e.name == "SUB").unwrap();
        assert!(sub.is_dir);

        let inner = fs.list_directory("/SUB").unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].name, "INNER.BIN");
        assert_eq!(inner[0].size_bytes, 4);
    }

    #[test]
    fn extracts_file_contents() {
        let img = build_image();
        let mut fs = FatxFs::new(Cursor::new(img)).unwrap();

        let dest = std::env::temp_dir().join("fatx_test_hello.txt");
        fs.extract_file("/HELLO.TXT", dest.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello fatx");
        let _ = std::fs::remove_file(&dest);

        let dest2 = std::env::temp_dir().join("fatx_test_inner.bin");
        fs.extract_file("/SUB/INNER.BIN", dest2.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&dest2).unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let _ = std::fs::remove_file(&dest2);
    }

    #[test]
    fn detects_xtaf_big_endian_header() {
        // Xbox 360 stores the signature byte-swapped ("XTAF") and is big-endian.
        let len = 0x8000usize;
        let mut img = vec![0u8; len];
        img[0..4].copy_from_slice(MAGIC_XTAF);
        img[8..12].copy_from_slice(&1u32.to_be_bytes());
        img[12..16].copy_from_slice(&1u32.to_be_bytes());
        let part = parse_partition(&mut Cursor::new(&img), "P", 0, len as u64).unwrap();
        assert!(part.big_endian);
        assert_eq!(part.root_dir_first_cluster, 1);
    }
}
