// Nitro filesystem — the directory tree inside a Nintendo DS ROM.
//
// A DS ROM carries two tables. The FAT is a flat array of 8-byte (start, end)
// pairs, one per file, indexed by file ID. The FNT holds the names and the
// shape of the tree:
//
//   FNT directory table, 8 bytes per directory, root first:
//     [0x00] u32  offset of this directory's name subtable, from the FNT start
//     [0x04] u16  file ID of the first file in this directory
//     [0x06] u16  parent directory ID — for the root, the directory count
//
//   Name subtable, a run of records ending in a 0x00 byte:
//     0x01..0x7F  a file: the value is the name length, name follows
//     0x81..0xFF  a directory: (value & 0x7F) is the name length, and a u16
//                 directory ID follows the name
//
// File IDs are handed out in order within a directory, starting at that
// directory's first_file_id, which is why the subtable never stores them.
//
// ROM header fields used here:
//   [0x00] 12  game title      [0x0C] 4  game code
//   [0x40] u32 FNT offset      [0x44] u32 FNT size
//   [0x48] u32 FAT offset      [0x4C] u32 FAT size

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::DiscEntry;

const ROOT_DIR_ID: u16 = 0xF000;
/// A DS ROM cannot hold more directories than the FNT can address.
const MAX_DIRS: usize = 0x1000;

struct DirNode {
    name: String,
    parent: u16,
    files: Vec<(String, u16)>,
    subdirs: Vec<u16>,
}

pub struct NitroFs {
    file: File,
    pub title: String,
    pub game_code: String,
    dirs: BTreeMap<u16, DirNode>,
    fat: Vec<(u32, u32)>,
}

fn le16(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(d[p..p + 2].try_into().unwrap())
}
fn le32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
}

fn trim_nul(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

/// A DS ROM has no magic at offset 0, so identify it by its Nintendo logo CRC
/// and self-consistent table offsets.
pub fn is_nds_rom(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut head = [0u8; 0x160];
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    // 0x15C holds the CRC of the BIOS logo, which is the same on every retail
    // cart; 0x9E1A is the widely documented value.
    let logo_crc = le16(&head, 0x15C);
    let fnt = le32(&head, 0x40) as u64;
    let fnt_size = le32(&head, 0x44) as u64;
    let fat = le32(&head, 0x48) as u64;
    let fat_size = le32(&head, 0x4C) as u64;
    logo_crc == 0x9E1A
        && fnt >= 0x160
        && fat >= 0x160
        && fnt + fnt_size <= len
        && fat + fat_size <= len
        && fat_size % 8 == 0
        && fat_size > 0
}

impl NitroFs {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("Cannot open ROM: {e}"))?;
        let len = file.metadata().map_err(|e| format!("ROM: {e}"))?.len();

        let mut head = [0u8; 0x160];
        file.read_exact(&mut head).map_err(|e| format!("ROM header: {e}"))?;

        let title = trim_nul(&head[0x00..0x0C]);
        let game_code = trim_nul(&head[0x0C..0x10]);
        let fnt_offset = le32(&head, 0x40) as u64;
        let fnt_size = le32(&head, 0x44) as usize;
        let fat_offset = le32(&head, 0x48) as u64;
        let fat_size = le32(&head, 0x4C) as usize;

        if fnt_size < 8 || fat_size < 8 || fat_size % 8 != 0 {
            return Err("Not a DS ROM (filesystem tables are missing or malformed)".into());
        }
        if fnt_offset + fnt_size as u64 > len || fat_offset + fat_size as u64 > len {
            return Err("DS ROM: filesystem tables run past the end of the file".into());
        }

        let mut fnt = vec![0u8; fnt_size];
        file.seek(SeekFrom::Start(fnt_offset)).map_err(|e| format!("ROM seek: {e}"))?;
        file.read_exact(&mut fnt).map_err(|e| format!("ROM FNT: {e}"))?;

        let mut fat_raw = vec![0u8; fat_size];
        file.seek(SeekFrom::Start(fat_offset)).map_err(|e| format!("ROM seek: {e}"))?;
        file.read_exact(&mut fat_raw).map_err(|e| format!("ROM FAT: {e}"))?;
        let fat: Vec<(u32, u32)> = fat_raw
            .chunks_exact(8)
            .map(|c| (le32(c, 0), le32(c, 4)))
            .collect();

        // The root's parent field doubles as the directory count.
        let dir_count = (le16(&fnt, 6) as usize).clamp(1, MAX_DIRS).min(fnt_size / 8);
        let mut dirs: BTreeMap<u16, DirNode> = BTreeMap::new();

        for i in 0..dir_count {
            let base = i * 8;
            if base + 8 > fnt.len() {
                break;
            }
            let sub_offset = le32(&fnt, base) as usize;
            let mut file_id = le16(&fnt, base + 4);
            let parent = le16(&fnt, base + 6);
            let id = ROOT_DIR_ID | i as u16;

            let mut node = DirNode {
                name: String::new(),
                parent,
                files: Vec::new(),
                subdirs: Vec::new(),
            };

            let mut p = sub_offset;
            while p < fnt.len() {
                let kind = fnt[p];
                p += 1;
                if kind == 0 {
                    break;
                }
                let name_len = (kind & 0x7F) as usize;
                if p + name_len > fnt.len() {
                    break;
                }
                let name = String::from_utf8_lossy(&fnt[p..p + name_len]).into_owned();
                p += name_len;

                if kind & 0x80 != 0 {
                    if p + 2 > fnt.len() {
                        break;
                    }
                    let child = le16(&fnt, p);
                    p += 2;
                    node.subdirs.push(child);
                    // The child's own record supplies everything but its name.
                    dirs.entry(child).or_insert_with(|| DirNode {
                        name: String::new(),
                        parent: id,
                        files: Vec::new(),
                        subdirs: Vec::new(),
                    });
                    if let Some(c) = dirs.get_mut(&child) {
                        c.name = name;
                    }
                } else {
                    node.files.push((name, file_id));
                    file_id = file_id.wrapping_add(1);
                }
            }

            // A directory discovered as somebody's child already has its name.
            match dirs.get_mut(&id) {
                Some(existing) => {
                    existing.parent = node.parent;
                    existing.files = node.files;
                    existing.subdirs = node.subdirs;
                }
                None => {
                    dirs.insert(id, node);
                }
            }
        }

        if dirs.is_empty() {
            return Err("DS ROM: no directories found".into());
        }
        Ok(NitroFs { file, title, game_code, dirs, fat })
    }

    fn resolve(&self, path: &str) -> Option<u16> {
        let mut id = ROOT_DIR_ID;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let node = self.dirs.get(&id)?;
            id = *node
                .subdirs
                .iter()
                .find(|c| self.dirs.get(c).is_some_and(|d| d.name.eq_ignore_ascii_case(part)))?;
        }
        Some(id)
    }

    fn find_file(&self, path: &str) -> Option<u16> {
        let (dir, name) = match path.rfind('/') {
            Some(cut) => (&path[..cut], &path[cut + 1..]),
            None => ("", path),
        };
        let id = self.resolve(dir)?;
        self.dirs
            .get(&id)?
            .files
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, fid)| *fid)
    }

    fn size_of(&self, file_id: u16) -> u64 {
        match self.fat.get(file_id as usize) {
            Some((start, end)) => (*end).saturating_sub(*start) as u64,
            None => 0,
        }
    }

    pub fn list_directory(&self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let id = self
            .resolve(dir_path)
            .ok_or_else(|| format!("Not found: {dir_path}"))?;
        let node = self.dirs.get(&id).ok_or("DS ROM: directory is missing")?;

        let mut out: Vec<DiscEntry> = node
            .subdirs
            .iter()
            .filter_map(|c| self.dirs.get(c))
            .map(|d| DiscEntry {
                name: d.name.clone(),
                is_dir: true,
                lba: 0,
                size: 0,
                size_bytes: 0,
                modified: String::new(),
                deleted: false,
                is_xa: false,
            })
            .collect();

        out.extend(node.files.iter().map(|(name, fid)| {
            let size = self.size_of(*fid);
            DiscEntry {
                name: name.clone(),
                is_dir: false,
                lba: self.fat.get(*fid as usize).map(|f| f.0 / 2048).unwrap_or(0),
                size: size.min(u32::MAX as u64) as u32,
                size_bytes: size.min(u32::MAX as u64) as u32,
                modified: String::new(),
                deleted: false,
                is_xa: false,
            }
        }));
        Ok(out)
    }

    fn read_file(&mut self, file_id: u16) -> Result<Vec<u8>, String> {
        let (start, end) = *self
            .fat
            .get(file_id as usize)
            .ok_or_else(|| format!("DS ROM: file ID {file_id} is out of range"))?;
        if end < start {
            return Err("DS ROM: file has a negative length".into());
        }
        let mut buf = vec![0u8; (end - start) as usize];
        self.file
            .seek(SeekFrom::Start(start as u64))
            .map_err(|e| format!("ROM seek: {e}"))?;
        self.file.read_exact(&mut buf).map_err(|e| format!("ROM read: {e}"))?;
        Ok(buf)
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let id = self
            .find_file(file_path.trim_start_matches('/'))
            .ok_or_else(|| format!("Not found: {file_path}"))?;
        let bytes = self.read_file(id)?;
        File::create(dest_path)
            .map_err(|e| format!("Cannot create file: {e}"))?
            .write_all(&bytes)
            .map_err(|e| format!("Write error: {e}"))
    }

    pub fn extract_directory(&mut self, dir_path: &str, dest_path: &str) -> Result<(), String> {
        let root = self
            .resolve(dir_path)
            .ok_or_else(|| format!("Not found: {dir_path}"))?;
        std::fs::create_dir_all(dest_path).map_err(|e| format!("Cannot create directory: {e}"))?;

        // Walk with an explicit stack; a malformed ROM could otherwise describe a
        // cycle and recurse forever.
        let mut seen = std::collections::BTreeSet::new();
        let mut stack = vec![(root, dest_path.to_string())];
        while let Some((id, dest)) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(node) = self.dirs.get(&id) else { continue };
            let files: Vec<(String, u16)> = node.files.clone();
            let subdirs: Vec<u16> = node.subdirs.clone();

            for child in subdirs {
                let Some(c) = self.dirs.get(&child) else { continue };
                let out = format!("{dest}/{}", crate::sanitize_component(&c.name));
                std::fs::create_dir_all(&out).map_err(|e| format!("Cannot create directory: {e}"))?;
                stack.push((child, out));
            }
            for (name, fid) in files {
                let out = format!("{dest}/{}", crate::sanitize_component(&name));
                let bytes = self.read_file(fid)?;
                File::create(&out)
                    .map_err(|e| format!("Cannot create file: {e}"))?
                    .write_all(&bytes)
                    .map_err(|e| format!("Write error: {e}"))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a ROM with the tree:
    //   /banner.bin
    //   /data/level1.bin
    //   /data/sub/deep.bin
    fn synth(dir: &Path) -> (std::path::PathBuf, Vec<Vec<u8>>) {
        let contents: Vec<Vec<u8>> = vec![
            b"banner contents".to_vec(),
            b"level one payload".to_vec(),
            vec![0xAB; 300],
        ];

        // Lay the files out first so the FAT can point at them.
        let data_start = 0x4000usize;
        let mut data = Vec::new();
        let mut fat = Vec::new();
        for c in &contents {
            let start = data_start + data.len();
            data.extend_from_slice(c);
            fat.extend_from_slice(&(start as u32).to_le_bytes());
            fat.extend_from_slice(&((start + c.len()) as u32).to_le_bytes());
        }

        // Three directories: root (0xF000), data (0xF001), sub (0xF002).
        let mut subtables: Vec<Vec<u8>> = Vec::new();
        // root: banner.bin (file 0), data/ -> 0xF001
        let mut t0 = Vec::new();
        t0.push(10);
        t0.extend_from_slice(b"banner.bin");
        t0.push(0x80 | 4);
        t0.extend_from_slice(b"data");
        t0.extend_from_slice(&0xF001u16.to_le_bytes());
        t0.push(0);
        subtables.push(t0);
        // data: level1.bin (file 1), sub/ -> 0xF002
        let mut t1 = Vec::new();
        t1.push(10);
        t1.extend_from_slice(b"level1.bin");
        t1.push(0x80 | 3);
        t1.extend_from_slice(b"sub");
        t1.extend_from_slice(&0xF002u16.to_le_bytes());
        t1.push(0);
        subtables.push(t1);
        // sub: deep.bin (file 2)
        let mut t2 = Vec::new();
        t2.push(8);
        t2.extend_from_slice(b"deep.bin");
        t2.push(0);
        subtables.push(t2);

        let header_len = 3 * 8;
        let mut fnt = vec![0u8; header_len];
        let mut offsets = Vec::new();
        for t in &subtables {
            offsets.push(header_len + fnt.len() - header_len);
            let at = fnt.len();
            fnt.extend_from_slice(t);
            offsets.pop();
            offsets.push(at);
        }
        let first_ids = [0u16, 1, 2];
        let parents = [3u16, ROOT_DIR_ID, ROOT_DIR_ID | 1];
        for i in 0..3 {
            let b = i * 8;
            fnt[b..b + 4].copy_from_slice(&(offsets[i] as u32).to_le_bytes());
            fnt[b + 4..b + 6].copy_from_slice(&first_ids[i].to_le_bytes());
            fnt[b + 6..b + 8].copy_from_slice(&parents[i].to_le_bytes());
        }

        let fnt_offset = 0x1000usize;
        let fat_offset = 0x2000usize;
        let mut rom = vec![0u8; data_start + data.len()];
        rom[0x00..0x0C].copy_from_slice(b"TESTROM\0\0\0\0\0");
        rom[0x0C..0x10].copy_from_slice(b"ATSE");
        rom[0x15C..0x15E].copy_from_slice(&0x9E1Au16.to_le_bytes());
        rom[0x40..0x44].copy_from_slice(&(fnt_offset as u32).to_le_bytes());
        rom[0x44..0x48].copy_from_slice(&(fnt.len() as u32).to_le_bytes());
        rom[0x48..0x4C].copy_from_slice(&(fat_offset as u32).to_le_bytes());
        rom[0x4C..0x50].copy_from_slice(&(fat.len() as u32).to_le_bytes());
        rom[fnt_offset..fnt_offset + fnt.len()].copy_from_slice(&fnt);
        rom[fat_offset..fat_offset + fat.len()].copy_from_slice(&fat);
        rom[data_start..].copy_from_slice(&data);

        let p = dir.join("test.nds");
        std::fs::write(&p, rom).unwrap();
        (p, contents)
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dx_nds_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn walks_the_tree_and_reads_files() {
        let d = scratch("tree");
        let (p, contents) = synth(&d);
        assert!(is_nds_rom(&p));

        let mut fs = NitroFs::open(&p).unwrap();
        assert_eq!(fs.title, "TESTROM");
        assert_eq!(fs.game_code, "ATSE");

        let root = fs.list_directory("/").unwrap();
        let names: Vec<_> = root.iter().map(|e| (e.name.as_str(), e.is_dir)).collect();
        assert_eq!(names, vec![("data", true), ("banner.bin", false)]);
        assert_eq!(root.iter().find(|e| e.name == "banner.bin").unwrap().size_bytes as usize, contents[0].len());

        let sub = fs.list_directory("/data/sub").unwrap();
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].name, "deep.bin");

        let out = d.join("deep.out");
        fs.extract_file("/data/sub/deep.bin", out.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), contents[2]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn extracts_the_whole_tree() {
        let d = scratch("extract");
        let (p, contents) = synth(&d);
        let mut fs = NitroFs::open(&p).unwrap();
        let dest = d.join("out");
        fs.extract_directory("/", dest.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(dest.join("banner.bin")).unwrap(), contents[0]);
        assert_eq!(std::fs::read(dest.join("data/level1.bin")).unwrap(), contents[1]);
        assert_eq!(std::fs::read(dest.join("data/sub/deep.bin")).unwrap(), contents[2]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_files_that_are_not_ds_roms() {
        let d = scratch("reject");
        let p = d.join("nope.nds");
        std::fs::write(&p, vec![0u8; 0x400]).unwrap();
        assert!(!is_nds_rom(&p), "a blank file has no logo CRC");
        assert!(NitroFs::open(&p).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
