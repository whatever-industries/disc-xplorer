// VPK (Valve Pak) archive reader.
//
// The directory is a three-level run of NUL-terminated strings — extension,
// then path, then file name — which is why a VPK groups everything by type
// rather than by folder. Each file record is:
//
//   u32 CRC-32
//   u16 preload length   — bytes stored inline, right here in the directory
//   u16 archive index    — 0x7FFF means "in this file", otherwise the sibling
//                          numbered archive
//   u32 entry offset
//   u32 entry length
//   u16 terminator = 0xFFFF
//   [preload length] bytes
//
// A file's contents are its preload bytes followed by `entry_length` bytes from
// wherever the archive index points, so small files often live entirely in the
// directory with no data section at all.
//
//   Header: u32 signature = 0x55AA1234, u32 version.
//   v1 adds u32 tree size (12 bytes total).
//   v2 adds four more section sizes (28 bytes total).
//
// An extension or path of " " means "none" and "root" respectively.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::DiscEntry;

const SIGNATURE: u32 = 0x55AA_1234;
const IN_THIS_FILE: u16 = 0x7FFF;
const TERMINATOR: u16 = 0xFFFF;

struct Entry {
    path: String,
    preload: Vec<u8>,
    archive_index: u16,
    offset: u32,
    length: u32,
}

impl Entry {
    fn size(&self) -> u64 {
        self.preload.len() as u64 + self.length as u64
    }
}

pub struct VpkArchive {
    dir_path: PathBuf,
    /// Where the data section begins in this file, for entries marked 0x7FFF.
    data_start: u64,
    files: BTreeMap<String, Entry>,
    dirs: BTreeSet<String>,
}

fn le16(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(d[p..p + 2].try_into().unwrap())
}
fn le32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
}

/// Read a NUL-terminated string, advancing past its terminator.
fn cstring(d: &[u8], p: &mut usize) -> Option<String> {
    let start = *p;
    let end = d[start..].iter().position(|&b| b == 0)? + start;
    let s = String::from_utf8_lossy(&d[start..end]).into_owned();
    *p = end + 1;
    Some(s)
}

pub fn is_vpk(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut head = [0u8; 8];
    f.read_exact(&mut head).is_ok() && le32(&head, 0) == SIGNATURE
}

impl VpkArchive {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("Cannot open VPK: {e}"))?;
        let mut head = [0u8; 28];
        let read = file.read(&mut head).map_err(|e| format!("VPK header: {e}"))?;
        if read < 12 || le32(&head, 0) != SIGNATURE {
            return Err("Not a VPK archive".into());
        }
        let version = le32(&head, 4);
        let tree_size = le32(&head, 8) as usize;
        let header_len: u64 = match version {
            1 => 12,
            2 => 28,
            v => return Err(format!("VPK: version {v} is not supported")),
        };
        if version == 2 && read < 28 {
            return Err("VPK: version 2 header is truncated".into());
        }

        let mut tree = vec![0u8; tree_size];
        file.seek(SeekFrom::Start(header_len))
            .map_err(|e| format!("VPK seek: {e}"))?;
        file.read_exact(&mut tree)
            .map_err(|e| format!("VPK directory: {e}"))?;

        let mut files = BTreeMap::new();
        let mut dirs = BTreeSet::new();
        let mut p = 0usize;

        'outer: loop {
            let Some(ext) = cstring(&tree, &mut p) else { break };
            if ext.is_empty() {
                break;
            }
            loop {
                let Some(dir) = cstring(&tree, &mut p) else { break 'outer };
                if dir.is_empty() {
                    break;
                }
                loop {
                    let Some(name) = cstring(&tree, &mut p) else { break 'outer };
                    if name.is_empty() {
                        break;
                    }
                    if p + 18 > tree.len() {
                        break 'outer;
                    }
                    let _crc = le32(&tree, p);
                    let preload_len = le16(&tree, p + 4) as usize;
                    let archive_index = le16(&tree, p + 6);
                    let offset = le32(&tree, p + 8);
                    let length = le32(&tree, p + 12);
                    let terminator = le16(&tree, p + 16);
                    p += 18;
                    if terminator != TERMINATOR {
                        return Err("VPK: directory entry is missing its terminator".into());
                    }
                    if p + preload_len > tree.len() {
                        break 'outer;
                    }
                    let preload = tree[p..p + preload_len].to_vec();
                    p += preload_len;

                    // " " is the archive's way of writing "none" for both fields.
                    let leaf = if ext == " " { name.clone() } else { format!("{name}.{ext}") };
                    let full = if dir == " " || dir.is_empty() {
                        format!("/{leaf}")
                    } else {
                        format!("/{}/{leaf}", dir.trim_matches('/'))
                    };

                    let mut parent = full.as_str();
                    while let Some(cut) = parent.rfind('/') {
                        if cut == 0 {
                            break;
                        }
                        parent = &parent[..cut];
                        dirs.insert(parent.to_string());
                    }

                    files.insert(
                        full.clone(),
                        Entry { path: full, preload, archive_index, offset, length },
                    );
                }
            }
        }

        if files.is_empty() {
            return Err("VPK: the directory holds no files".into());
        }
        Ok(VpkArchive {
            dir_path: path.to_path_buf(),
            data_start: header_len + tree_size as u64,
            files,
            dirs,
        })
    }

    /// Numbered archives sit beside the directory: `pak01_dir.vpk` is served by
    /// `pak01_000.vpk`, `pak01_001.vpk` and so on.
    fn archive_path(&self, index: u16) -> Result<PathBuf, String> {
        let name = self
            .dir_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("VPK: cannot read the archive name")?;
        let stem = name
            .strip_suffix(".vpk")
            .ok_or("VPK: archive name does not end in .vpk")?;
        let base = stem.strip_suffix("_dir").unwrap_or(stem);
        Ok(self
            .dir_path
            .with_file_name(format!("{base}_{index:03}.vpk")))
    }

    pub fn list_directory(&self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let trimmed = dir_path.trim_end_matches('/');
        let prefix = if trimmed.is_empty() { "/".to_string() } else { format!("{trimmed}/") };

        fn child<'a>(full: &'a str, prefix: &str) -> Option<&'a str> {
            let rest = full.strip_prefix(prefix)?;
            (!rest.is_empty() && !rest.contains('/')).then_some(rest)
        }

        let mut out: Vec<DiscEntry> = self
            .dirs
            .iter()
            .filter_map(|d| child(d, &prefix))
            .map(|name| DiscEntry {
                name: name.to_string(),
                is_dir: true,
                lba: 0,
                size: 0,
                size_bytes: 0,
                modified: String::new(),
                deleted: false,
                is_xa: false,
            })
            .collect();

        out.extend(self.files.values().filter_map(|e| {
            let name = child(&e.path, &prefix)?;
            let size = e.size();
            Some(DiscEntry {
                name: name.to_string(),
                is_dir: false,
                lba: 0,
                size: size.min(u32::MAX as u64) as u32,
                size_bytes: size.min(u32::MAX as u64) as u32,
                modified: String::new(),
                deleted: false,
                is_xa: false,
            })
        }));

        if out.is_empty() && !trimmed.is_empty() && !self.dirs.contains(trimmed) {
            return Err(format!("Not found: {dir_path}"));
        }
        Ok(out)
    }

    fn read_entry(&self, key: &str) -> Result<Vec<u8>, String> {
        let e = self.files.get(key).ok_or_else(|| format!("Not found: {key}"))?;
        let mut out = e.preload.clone();
        if e.length == 0 {
            return Ok(out);
        }

        let (path, at) = if e.archive_index == IN_THIS_FILE {
            (self.dir_path.clone(), self.data_start + e.offset as u64)
        } else {
            (self.archive_path(e.archive_index)?, e.offset as u64)
        };
        let mut f = File::open(&path)
            .map_err(|err| format!("Cannot open {}: {err}", path.display()))?;
        f.seek(SeekFrom::Start(at)).map_err(|err| format!("VPK seek: {err}"))?;
        let mut buf = vec![0u8; e.length as usize];
        f.read_exact(&mut buf).map_err(|err| format!("VPK read: {err}"))?;
        out.extend_from_slice(&buf);
        Ok(out)
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let key = if file_path.starts_with('/') {
            file_path.to_string()
        } else {
            format!("/{file_path}")
        };
        let bytes = self.read_entry(&key)?;
        File::create(dest_path)
            .map_err(|e| format!("Cannot create file: {e}"))?
            .write_all(&bytes)
            .map_err(|e| format!("Write error: {e}"))
    }

    pub fn extract_directory(&mut self, dir_path: &str, dest_path: &str) -> Result<(), String> {
        let trimmed = dir_path.trim_end_matches('/');
        let prefix = if trimmed.is_empty() { "/".to_string() } else { format!("{trimmed}/") };

        let targets: Vec<String> = self
            .files
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        if targets.is_empty() && !trimmed.is_empty() && !self.dirs.contains(trimmed) {
            return Err(format!("Not found: {dir_path}"));
        }
        std::fs::create_dir_all(dest_path).map_err(|e| format!("Cannot create directory: {e}"))?;

        for key in targets {
            let rel = &key[prefix.len()..];
            let safe: Vec<String> = rel.split('/').map(crate::sanitize_component).collect();
            let out = format!("{dest_path}/{}", safe.join("/"));
            if let Some(parent) = Path::new(&out).parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create directory: {e}"))?;
            }
            let bytes = self.read_entry(&key)?;
            File::create(&out)
                .map_err(|e| format!("Cannot create file: {e}"))?
                .write_all(&bytes)
                .map_err(|e| format!("Write error: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nul_terminated_strings() {
        let d = b"ext\0dir\0\0";
        let mut p = 0;
        assert_eq!(cstring(d, &mut p).as_deref(), Some("ext"));
        assert_eq!(cstring(d, &mut p).as_deref(), Some("dir"));
        assert_eq!(cstring(d, &mut p).as_deref(), Some(""));
        assert_eq!(cstring(d, &mut p), None, "past the end");
    }

    #[test]
    fn names_numbered_archives_beside_the_directory() {
        let a = VpkArchive {
            dir_path: PathBuf::from("/games/pak01_dir.vpk"),
            data_start: 0,
            files: BTreeMap::new(),
            dirs: BTreeSet::new(),
        };
        assert_eq!(a.archive_path(0).unwrap(), PathBuf::from("/games/pak01_000.vpk"));
        assert_eq!(a.archive_path(12).unwrap(), PathBuf::from("/games/pak01_012.vpk"));

        // A single-file VPK has no "_dir" to strip.
        let b = VpkArchive {
            dir_path: PathBuf::from("/games/misc.vpk"),
            data_start: 0,
            files: BTreeMap::new(),
            dirs: BTreeSet::new(),
        };
        assert_eq!(b.archive_path(1).unwrap(), PathBuf::from("/games/misc_001.vpk"));
    }
}
