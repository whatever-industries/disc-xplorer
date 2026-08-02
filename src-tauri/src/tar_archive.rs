// TAR archive reader, including the gzip/bzip2/xz/zstd-wrapped variants.
//
// A tar is a flat run of 512-byte headers, each followed by its file's bytes
// padded up to the next 512-byte boundary. There is no index, so the whole
// archive has to be walked once to know what is in it.
//
//   Header (512 bytes, numbers are NUL/space-terminated octal ASCII):
//     [0x000] 100  name
//     [0x064] 8    mode        [0x06C] 8 uid       [0x074] 8 gid
//     [0x07C] 12   size        [0x088] 12 mtime    [0x094] 8 checksum
//     [0x09C] 1    type flag   [0x09D] 100 link name
//     [0x101] 6    magic "ustar"                   [0x107] 2 version
//     [0x159] 155  prefix — prepended to name, which is how ustar stores
//                  paths longer than 100 bytes
//
// Two extensions matter in practice: GNU's 'L' record, which puts a long name
// in its own data block ahead of the entry it names, and pax 'x'/'g' records,
// whose "length key=value" pairs can carry both the path and the size.
//
// A compressed tar has to be decompressed before it can be walked, so those are
// held in memory; a plain tar is read from the file on demand.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::DiscEntry;

const BLOCK: usize = 512;
/// Ceiling on a compressed tar, which must be decompressed whole before it can
/// be indexed. Plain tars are read from disk and have no such limit.
const MAX_IN_MEMORY: u64 = 2 * 1024 * 1024 * 1024;

struct Entry {
    path: String,
    offset: u64,
    size: u64,
    modified: String,
}

enum Source {
    OnDisk(File),
    InMemory(Vec<u8>),
}

pub struct TarArchive {
    source: Source,
    files: BTreeMap<String, Entry>,
    dirs: BTreeSet<String>,
}

/// Numeric fields are octal ASCII, but GNU writes large values as binary with
/// the top bit of the first byte set.
fn parse_number(field: &[u8]) -> u64 {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return field
            .iter()
            .fold(0u64, |acc, &b| (acc << 8) | (b & 0x7F) as u64);
    }
    let text: String = field
        .iter()
        .take_while(|&&b| b != 0 && b != b' ')
        .map(|&b| b as char)
        .collect();
    u64::from_str_radix(text.trim(), 8).unwrap_or(0)
}

fn parse_string(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn format_mtime(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    // Civil-from-days, so no date library is needed for a display string.
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}

fn normalise(name: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in name.replace('\\', "/").split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            p => parts.push(p.to_string()),
        }
    }
    (!parts.is_empty()).then(|| format!("/{}", parts.join("/")))
}

/// Decompress by signature rather than by file extension, since `.tgz`, `.tar.gz`
/// and a bare `.tar` that is actually gzipped all turn up in the wild.
fn decompress_if_needed(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut f = File::open(path).map_err(|e| format!("Cannot open archive: {e}"))?;
    let mut magic = [0u8; 6];
    let n = f.read(&mut magic).map_err(|e| format!("Read error: {e}"))?;
    if n < 4 {
        return Err("Archive is too small".into());
    }
    f.seek(SeekFrom::Start(0)).map_err(|e| format!("Seek error: {e}"))?;

    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut out = Vec::new();
    let kind = if magic[0] == 0x1F && magic[1] == 0x8B {
        "gzip"
    } else if &magic[0..3] == b"BZh" {
        "bzip2"
    } else if magic[0..6] == [0xFD, b'7', b'z', b'X', b'Z', 0x00] {
        "xz"
    } else if magic[0..4] == [0x28, 0xB5, 0x2F, 0xFD] {
        "zstd"
    } else {
        return Ok(None);
    };

    if size > MAX_IN_MEMORY {
        return Err(format!(
            "Compressed archive is too large to index ({size} bytes); decompress it first"
        ));
    }
    match kind {
        "gzip" => flate2::read::GzDecoder::new(f)
            .read_to_end(&mut out)
            .map(|_| ())
            .map_err(|e| format!("gzip: {e}")),
        "bzip2" => bzip2_rs::DecoderReader::new(f)
            .read_to_end(&mut out)
            .map(|_| ())
            .map_err(|e| format!("bzip2: {e}")),
        "xz" => {
            let mut buf = std::io::BufReader::new(f);
            lzma_rs::xz_decompress(&mut buf, &mut out).map_err(|e| format!("xz: {e}"))
        }
        _ => zstd::stream::copy_decode(f, &mut out).map_err(|e| format!("zstd: {e}")),
    }?;
    Ok(Some(out))
}

impl TarArchive {
    pub fn open(path: &Path) -> Result<Self, String> {
        let source = match decompress_if_needed(path)? {
            Some(bytes) => Source::InMemory(bytes),
            None => Source::OnDisk(File::open(path).map_err(|e| format!("Cannot open TAR: {e}"))?),
        };
        let mut archive = TarArchive { source, files: BTreeMap::new(), dirs: BTreeSet::new() };
        archive.index()?;
        if archive.files.is_empty() && archive.dirs.is_empty() {
            return Err("Not a TAR archive (no usable entries)".into());
        }
        Ok(archive)
    }

    fn total_len(&self) -> u64 {
        match &self.source {
            Source::InMemory(v) => v.len() as u64,
            Source::OnDisk(f) => f.metadata().map(|m| m.len()).unwrap_or(0),
        }
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        match &mut self.source {
            Source::InMemory(v) => {
                let s = offset as usize;
                let e = (s + len).min(v.len());
                if s >= v.len() {
                    return Err("TAR: read past the end of the archive".into());
                }
                Ok(v[s..e].to_vec())
            }
            Source::OnDisk(f) => {
                let mut buf = vec![0u8; len];
                f.seek(SeekFrom::Start(offset)).map_err(|e| format!("TAR seek: {e}"))?;
                f.read_exact(&mut buf).map_err(|e| format!("TAR read: {e}"))?;
                Ok(buf)
            }
        }
    }

    fn index(&mut self) -> Result<(), String> {
        let total = self.total_len();
        let mut pos = 0u64;
        // Carried between records by the GNU 'L' and pax 'x' extensions.
        let mut pending_name: Option<String> = None;
        let mut pending_size: Option<u64> = None;

        while pos + BLOCK as u64 <= total {
            let header = self.read_at(pos, BLOCK)?;
            // Two zero blocks mark the end; a single one ends the walk too.
            if header.iter().all(|&b| b == 0) {
                break;
            }
            pos += BLOCK as u64;

            let mut name = parse_string(&header[0..100]);
            let size = parse_number(&header[124..136]);
            let mtime = parse_number(&header[136..148]);
            let type_flag = header[156];
            let prefix = parse_string(&header[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }

            let data_at = pos;
            let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
            pos += padded;

            match type_flag {
                // GNU long name: this record's data is the next entry's path.
                b'L' => {
                    let raw = self.read_at(data_at, size as usize)?;
                    pending_name = Some(parse_string(&raw));
                    continue;
                }
                // pax header: "length key=value\n" records.
                b'x' | b'g' => {
                    let raw = self.read_at(data_at, size as usize)?;
                    let text = String::from_utf8_lossy(&raw);
                    let mut rest = text.as_ref();
                    while let Some(sp) = rest.find(' ') {
                        let Ok(reclen) = rest[..sp].parse::<usize>() else { break };
                        if reclen > rest.len() || reclen <= sp {
                            break;
                        }
                        let kv = rest[sp + 1..reclen].trim_end_matches('\n');
                        if let Some((k, v)) = kv.split_once('=') {
                            match k {
                                "path" => pending_name = Some(v.to_string()),
                                "size" => pending_size = v.parse().ok(),
                                _ => {}
                            }
                        }
                        rest = &rest[reclen..];
                    }
                    continue;
                }
                _ => {}
            }

            if let Some(n) = pending_name.take() {
                name = n;
            }
            let size = pending_size.take().unwrap_or(size);

            let is_dir = type_flag == b'5' || name.ends_with('/');
            let Some(norm) = normalise(&name) else { continue };

            let mut parent = norm.as_str();
            while let Some(cut) = parent.rfind('/') {
                if cut == 0 {
                    break;
                }
                parent = &parent[..cut];
                self.dirs.insert(parent.to_string());
            }

            // Only regular files carry data worth extracting; links and devices
            // are listed by their directory entry alone.
            if is_dir {
                self.dirs.insert(norm);
            } else if matches!(type_flag, b'0' | 0) {
                self.files.insert(
                    norm.clone(),
                    Entry { path: norm, offset: data_at, size, modified: format_mtime(mtime) },
                );
            }
        }
        Ok(())
    }

    pub fn list_directory(&self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let base = normalise(dir_path).unwrap_or_default();
        let prefix = if base.is_empty() { "/".to_string() } else { format!("{base}/") };

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

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let key = normalise(file_path).ok_or_else(|| format!("Bad path: {file_path}"))?;
        let (offset, size) = {
            let e = self.files.get(&key).ok_or_else(|| format!("Not found: {file_path}"))?;
            (e.offset, e.size)
        };
        let bytes = self.read_at(offset, size as usize)?;
        File::create(dest_path)
            .map_err(|e| format!("Cannot create file: {e}"))?
            .write_all(&bytes)
            .map_err(|e| format!("Write error: {e}"))
    }

    pub fn extract_directory(&mut self, dir_path: &str, dest_path: &str) -> Result<(), String> {
        let base = normalise(dir_path).unwrap_or_default();
        let prefix = if base.is_empty() { "/".to_string() } else { format!("{base}/") };

        let targets: Vec<(String, u64, u64)> = self
            .files
            .values()
            .filter(|e| e.path.starts_with(&prefix))
            .map(|e| (e.path.clone(), e.offset, e.size))
            .collect();
        if targets.is_empty() && !base.is_empty() && !self.dirs.contains(&base) {
            return Err(format!("Not found: {dir_path}"));
        }
        std::fs::create_dir_all(dest_path).map_err(|e| format!("Cannot create directory: {e}"))?;

        for (key, offset, size) in targets {
            let rel = &key[prefix.len()..];
            let safe: Vec<String> = rel.split('/').map(crate::sanitize_component).collect();
            let out = format!("{dest_path}/{}", safe.join("/"));
            if let Some(parent) = Path::new(&out).parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create directory: {e}"))?;
            }
            let bytes = self.read_at(offset, size as usize)?;
            File::create(&out)
                .map_err(|e| format!("Cannot create file: {e}"))?
                .write_all(&bytes)
                .map_err(|e| format!("Write error: {e}"))?;
        }
        Ok(())
    }
}

/// A tar has no magic at offset 0, so identify it by the ustar marker in the
/// first header, or by a recognised compression wrapper.
pub fn is_tar(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut head = [0u8; 265];
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    if &head[257..262] == b"ustar" {
        return true;
    }
    head[0..2] == [0x1F, 0x8B]
        || &head[0..3] == b"BZh"
        || head[0..6] == [0xFD, b'7', b'z', b'X', b'Z', 0x00]
        || head[0..4] == [0x28, 0xB5, 0x2F, 0xFD]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_octal_and_gnu_binary_numbers() {
        assert_eq!(parse_number(b"0000144\0"), 100);
        assert_eq!(parse_number(b"00000000000 "), 0);
        // GNU base-256: high bit set, remainder big-endian.
        assert_eq!(parse_number(&[0x80, 0, 0, 0, 0, 0, 0, 1]), 1);
        assert_eq!(parse_number(&[0x80, 0, 0, 0, 0, 0, 1, 0]), 256);
    }

    #[test]
    fn formats_mtimes_without_a_date_library() {
        assert_eq!(format_mtime(0), "");
        assert_eq!(format_mtime(1_000_000_000), "2001-09-09 01:46:40");
        assert_eq!(format_mtime(1_700_000_000), "2023-11-14 22:13:20");
    }

    #[test]
    fn refuses_traversal_names() {
        assert_eq!(normalise("./a/b").as_deref(), Some("/a/b"));
        assert_eq!(normalise("../etc/passwd"), None);
        assert_eq!(normalise("a/../../b"), None);
    }
}
