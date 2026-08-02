// ZIP (PKZIP) archive reader.
//
// Read from the central directory at the end of the file rather than by walking
// local headers forward: the central directory is the authoritative index, and
// it is what every other tool trusts when the two disagree.
//
//   End of central directory record (signature 50 4B 05 06):
//     [0x00] u32  signature
//     [0x08] u16  entries on this disk
//     [0x0A] u16  total entries
//     [0x0C] u32  central directory size
//     [0x10] u32  central directory offset
//     [0x14] u16  comment length
//
//   Central directory entry (signature 50 4B 01 02, 46 bytes then name/extra):
//     [0x0A] u16  compression method
//     [0x0C] u16  modified time      [0x0E] u16 modified date
//     [0x10] u32  CRC-32
//     [0x14] u32  compressed size    [0x18] u32 uncompressed size
//     [0x1C] u16  name length        [0x1E] u16 extra length
//     [0x20] u16  comment length
//     [0x2A] u32  offset of the local header
//
// Sizes and offsets of 0xFFFFFFFF mean the real value is in a Zip64 extra
// field, which is how archives over 4 GB are represented.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::DiscEntry;

const EOCD_SIG: u32 = 0x0605_4B50;
const EOCD64_SIG: u32 = 0x0606_4B50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4B50;
const CENTRAL_SIG: u32 = 0x0201_4B50;
const LOCAL_SIG: u32 = 0x0403_4B50;

const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const METHOD_BZIP2: u16 = 12;
const METHOD_LZMA: u16 = 14;
const METHOD_ZSTD: u16 = 93;

const ZIP64_MARKER32: u32 = 0xFFFF_FFFF;

struct Entry {
    path: String,
    size: u64,
    compressed_size: u64,
    method: u16,
    local_offset: u64,
    modified: String,
}

pub struct ZipArchive {
    file: File,
    files: BTreeMap<String, Entry>,
    dirs: BTreeSet<String>,
}

fn le16(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(d[p..p + 2].try_into().unwrap())
}
fn le32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
}
fn le64(d: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(d[p..p + 8].try_into().unwrap())
}

/// MS-DOS packed date/time, as ZIP has stored timestamps since 1989.
fn dos_datetime(time: u16, date: u16) -> String {
    let (y, mo, d) = (1980 + (date >> 9), (date >> 5) & 0xF, date & 0x1F);
    let (h, mi, s) = (time >> 11, (time >> 5) & 0x3F, (time & 0x1F) * 2);
    if date == 0 {
        return String::new();
    }
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Normalise a stored name to the app's leading-slash convention, rejecting the
/// traversal forms an archive can carry.
fn normalise(name: &str) -> Option<String> {
    let cleaned = name.replace('\\', "/");
    let mut parts = Vec::new();
    for part in cleaned.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| format!("/{}", parts.join("/")))
}

impl ZipArchive {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("Cannot open ZIP: {e}"))?;
        let len = file.metadata().map_err(|e| format!("ZIP: {e}"))?.len();

        // The record sits at the end but may be followed by up to 64 KB of
        // comment, so scan backwards for its signature.
        let tail_len = len.min(66 * 1024) as usize;
        let mut tail = vec![0u8; tail_len];
        file.seek(SeekFrom::Start(len - tail_len as u64))
            .map_err(|e| format!("ZIP seek: {e}"))?;
        file.read_exact(&mut tail).map_err(|e| format!("ZIP: {e}"))?;

        let eocd = (0..tail_len.saturating_sub(21))
            .rev()
            .find(|&i| le32(&tail, i) == EOCD_SIG)
            .ok_or("Not a ZIP archive (no end of central directory record)")?;

        let mut count = le16(&tail, eocd + 0x0A) as u64;
        let mut cd_size = le32(&tail, eocd + 0x0C) as u64;
        let mut cd_offset = le32(&tail, eocd + 0x10) as u64;

        // Zip64: the 32-bit fields are saturated and the real ones live in a
        // separate record, found through a locator just before the EOCD.
        if cd_offset == ZIP64_MARKER32 as u64 || cd_size == ZIP64_MARKER32 as u64 || count == 0xFFFF {
            let loc = (0..eocd)
                .rev()
                .find(|&i| le32(&tail, i) == EOCD64_LOCATOR_SIG)
                .ok_or("ZIP: Zip64 sizes but no Zip64 locator")?;
            let eocd64_offset = le64(&tail, loc + 8);
            let mut rec = [0u8; 56];
            file.seek(SeekFrom::Start(eocd64_offset))
                .map_err(|e| format!("ZIP seek: {e}"))?;
            file.read_exact(&mut rec).map_err(|e| format!("ZIP Zip64 record: {e}"))?;
            if le32(&rec, 0) != EOCD64_SIG {
                return Err("ZIP: Zip64 record signature is wrong".into());
            }
            count = le64(&rec, 0x20);
            cd_size = le64(&rec, 0x28);
            cd_offset = le64(&rec, 0x30);
        }

        if cd_offset + cd_size > len {
            return Err("ZIP: central directory runs past the end of the file".into());
        }
        let mut cd = vec![0u8; cd_size as usize];
        file.seek(SeekFrom::Start(cd_offset))
            .map_err(|e| format!("ZIP seek: {e}"))?;
        file.read_exact(&mut cd).map_err(|e| format!("ZIP central directory: {e}"))?;

        let mut files = BTreeMap::new();
        let mut dirs = BTreeSet::new();
        let mut p = 0usize;
        for _ in 0..count {
            if p + 46 > cd.len() || le32(&cd, p) != CENTRAL_SIG {
                break;
            }
            let method = le16(&cd, p + 0x0A);
            let time = le16(&cd, p + 0x0C);
            let date = le16(&cd, p + 0x0E);
            let mut compressed_size = le32(&cd, p + 0x14) as u64;
            let mut size = le32(&cd, p + 0x18) as u64;
            let name_len = le16(&cd, p + 0x1C) as usize;
            let extra_len = le16(&cd, p + 0x1E) as usize;
            let comment_len = le16(&cd, p + 0x20) as usize;
            let mut local_offset = le32(&cd, p + 0x2A) as u64;

            let name_at = p + 46;
            if name_at + name_len + extra_len + comment_len > cd.len() {
                break;
            }
            let raw_name = String::from_utf8_lossy(&cd[name_at..name_at + name_len]).into_owned();

            // Zip64 extra field: present values appear in a fixed order, but only
            // for the fields that were saturated.
            if size == ZIP64_MARKER32 as u64
                || compressed_size == ZIP64_MARKER32 as u64
                || local_offset == ZIP64_MARKER32 as u64
            {
                let extra = &cd[name_at + name_len..name_at + name_len + extra_len];
                let mut q = 0usize;
                while q + 4 <= extra.len() {
                    let tag = le16(extra, q);
                    let flen = le16(extra, q + 2) as usize;
                    if tag == 0x0001 && q + 4 + flen <= extra.len() {
                        let mut r = q + 4;
                        for target in [&mut size, &mut compressed_size, &mut local_offset] {
                            if *target == ZIP64_MARKER32 as u64 && r + 8 <= q + 4 + flen {
                                *target = le64(extra, r);
                                r += 8;
                            }
                        }
                        break;
                    }
                    q += 4 + flen;
                }
            }

            p = name_at + name_len + extra_len + comment_len;

            let is_dir = raw_name.ends_with('/') || raw_name.ends_with('\\');
            let Some(norm) = normalise(&raw_name) else { continue };

            // Every parent of an entry is a directory, whether or not the archive
            // bothered to store one: plenty of writers omit them.
            let mut parent = norm.as_str();
            while let Some(cut) = parent.rfind('/') {
                if cut == 0 {
                    break;
                }
                parent = &parent[..cut];
                dirs.insert(parent.to_string());
            }

            if is_dir {
                dirs.insert(norm);
            } else {
                files.insert(
                    norm.clone(),
                    Entry {
                        path: norm,
                        size,
                        compressed_size,
                        method,
                        local_offset,
                        modified: dos_datetime(time, date),
                    },
                );
            }
        }

        if files.is_empty() && dirs.is_empty() {
            return Err("ZIP: the central directory holds no usable entries".into());
        }
        Ok(ZipArchive { file, files, dirs })
    }

    pub fn list_directory(&self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let base = normalise(dir_path).unwrap_or_else(|| "/".to_string());
        let base = if base == "/" { String::new() } else { base };
        let prefix = format!("{base}/");

        // An immediate child is one whose remaining path has no further slash.
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
            Some(DiscEntry {
                name: name.to_string(),
                is_dir: false,
                lba: 0,
                size: e.size.min(u32::MAX as u64) as u32,
                size_bytes: e.size.min(u32::MAX as u64) as u32,
                modified: e.modified.clone(),
                deleted: false,
                is_xa: false,
            })
        }));

        if out.is_empty() && !base.is_empty() && !self.dirs.contains(&base) {
            return Err(format!("Not found: {dir_path}"));
        }
        Ok(out)
    }

    /// Read one entry's bytes. The local header is re-read because its name and
    /// extra lengths can differ from the central directory's, and the data
    /// starts immediately after them.
    fn read_entry(&mut self, e_path: &str) -> Result<Vec<u8>, String> {
        let (offset, compressed_size, size, method) = {
            let e = self
                .files
                .get(e_path)
                .ok_or_else(|| format!("Not found: {e_path}"))?;
            (e.local_offset, e.compressed_size, e.size, e.method)
        };

        let mut header = [0u8; 30];
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| format!("ZIP seek: {e}"))?;
        self.file
            .read_exact(&mut header)
            .map_err(|e| format!("ZIP local header: {e}"))?;
        if le32(&header, 0) != LOCAL_SIG {
            return Err("ZIP: local header signature is wrong".into());
        }
        let data_at = offset + 30 + le16(&header, 26) as u64 + le16(&header, 28) as u64;

        let mut raw = vec![0u8; compressed_size as usize];
        self.file
            .seek(SeekFrom::Start(data_at))
            .map_err(|e| format!("ZIP seek: {e}"))?;
        self.file
            .read_exact(&mut raw)
            .map_err(|e| format!("ZIP read: {e}"))?;

        let want = size as usize;
        match method {
            METHOD_STORE => Ok(raw),
            METHOD_DEFLATE => {
                let mut out = Vec::with_capacity(want);
                flate2::read::DeflateDecoder::new(&raw[..])
                    .read_to_end(&mut out)
                    .map_err(|e| format!("ZIP: deflate failed: {e}"))?;
                Ok(out)
            }
            METHOD_BZIP2 => {
                let mut out = Vec::with_capacity(want);
                bzip2_rs::DecoderReader::new(&raw[..])
                    .read_to_end(&mut out)
                    .map_err(|e| format!("ZIP: bzip2 failed: {e}"))?;
                Ok(out)
            }
            METHOD_ZSTD => zstd::bulk::decompress(&raw, want.max(1))
                .map_err(|e| format!("ZIP: zstd failed: {e}")),
            METHOD_LZMA => {
                // ZIP's LZMA entries carry a 4-byte version/properties-size prefix
                // ahead of the usual 5 property bytes, and no size field.
                if raw.len() < 9 {
                    return Err("ZIP: LZMA entry is too short".into());
                }
                let mut stream = Vec::with_capacity(13 + raw.len());
                stream.extend_from_slice(&raw[4..9]);
                stream.extend_from_slice(&(size).to_le_bytes());
                stream.extend_from_slice(&raw[9..]);
                let mut out = Vec::with_capacity(want);
                lzma_rs::lzma_decompress(&mut &stream[..], &mut out)
                    .map_err(|e| format!("ZIP: LZMA failed: {e}"))?;
                Ok(out)
            }
            other => Err(format!("ZIP: unsupported compression method {other}")),
        }
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let key = normalise(file_path).ok_or_else(|| format!("Bad path: {file_path}"))?;
        let bytes = self.read_entry(&key)?;
        File::create(dest_path)
            .map_err(|e| format!("Cannot create file: {e}"))?
            .write_all(&bytes)
            .map_err(|e| format!("Write error: {e}"))
    }

    pub fn extract_directory(&mut self, dir_path: &str, dest_path: &str) -> Result<(), String> {
        let base = normalise(dir_path).unwrap_or_default();
        let prefix = if base.is_empty() { "/".to_string() } else { format!("{base}/") };

        let targets: Vec<String> = self
            .files
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        if targets.is_empty() && !base.is_empty() && !self.dirs.contains(&base) {
            return Err(format!("Not found: {dir_path}"));
        }
        std::fs::create_dir_all(dest_path).map_err(|e| format!("Cannot create directory: {e}"))?;

        for key in targets {
            let rel = &key[prefix.len()..];
            let safe: Vec<String> = rel.split('/').map(crate::sanitize_component).collect();
            let out = format!("{dest_path}/{}", safe.join("/"));
            if let Some(parent) = Path::new(&out).parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create directory: {e}"))?;
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

pub fn is_zip(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut magic = [0u8; 4];
    // An empty archive starts with the end-of-central-directory record instead.
    f.read_exact(&mut magic).is_ok()
        && (le32(&magic, 0) == LOCAL_SIG || le32(&magic, 0) == EOCD_SIG)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute_names() {
        assert_eq!(normalise("a/b.txt").as_deref(), Some("/a/b.txt"));
        assert_eq!(normalise("./a//b.txt").as_deref(), Some("/a/b.txt"));
        assert_eq!(normalise("a\\b.txt").as_deref(), Some("/a/b.txt"));
        assert_eq!(normalise("../../etc/passwd"), None, "traversal must be refused");
        assert_eq!(normalise("a/../../b"), None);
        assert_eq!(normalise(""), None);
    }

    #[test]
    fn decodes_dos_timestamps() {
        // 2024-05-17 14:30:44
        let date = ((2024 - 1980) << 9) | (5 << 5) | 17;
        let time = (14 << 11) | (30 << 5) | 22;
        assert_eq!(dos_datetime(time, date), "2024-05-17 14:30:44");
        assert_eq!(dos_datetime(0, 0), "", "an unset date shows nothing");
    }
}
