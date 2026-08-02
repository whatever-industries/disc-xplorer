// Microsoft Cabinet (.cab) archive reader.
//
// A cabinet groups its files into "folders", which are compression streams
// rather than directories: every file in a folder is concatenated and the whole
// run compressed together, so a file is located by its byte offset into the
// folder's decompressed output. Directory structure, where there is any, lives
// in the file names themselves.
//
//   CFHEADER (36 bytes):
//     [0x00] u32 "MSCF"          [0x08] u32 total cabinet size
//     [0x10] u32 offset of the first CFFILE
//     [0x18] u8  version minor   [0x19] u8  version major
//     [0x1A] u16 folder count    [0x1C] u16 file count
//     [0x1E] u16 flags           [0x20] u16 set ID   [0x22] u16 cabinet index
//     Optional, per flags: reserve sizes and blob, previous/next cabinet names.
//
//   CFFOLDER (8 bytes + per-folder reserve):
//     u32 offset of the folder's first CFDATA, u16 block count,
//     u16 compression type (low 4 bits: 0 none, 1 MSZIP, 2 Quantum, 3 LZX)
//
//   CFFILE (16 bytes + NUL-terminated name):
//     u32 uncompressed size, u32 offset within the folder, u16 folder index,
//     u16 date, u16 time, u16 attributes
//
//   CFDATA (8 bytes + per-block reserve + payload):
//     u32 checksum, u16 compressed size, u16 uncompressed size
//
// MSZIP is the part that needs care: each block is a raw deflate stream behind a
// "CK" signature, and it is primed with the previous block's output as its
// dictionary. Blocks therefore cannot be decompressed independently — they are
// inflated into one shared buffer so back-references reach into what came
// before.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;

use crate::DiscEntry;

const MAGIC: &[u8; 4] = b"MSCF";
const FLAG_PREV_CABINET: u16 = 0x0001;
const FLAG_NEXT_CABINET: u16 = 0x0002;
const FLAG_RESERVE_PRESENT: u16 = 0x0004;

const COMPRESS_NONE: u16 = 0;
const COMPRESS_MSZIP: u16 = 1;
const COMPRESS_QUANTUM: u16 = 2;
const COMPRESS_LZX: u16 = 3;

/// A file whose name is stored UTF-8 rather than in the OEM code page.
const ATTR_NAME_IS_UTF: u16 = 0x80;

struct Folder {
    data_start: u64,
    blocks: u16,
    compression: u16,
}

struct FileRec {
    path: String,
    size: u32,
    folder_offset: u32,
    folder: u16,
    modified: String,
}

pub struct CabArchive {
    file: File,
    folders: Vec<Folder>,
    files: Vec<FileRec>,
    dirs: BTreeSet<String>,
    data_reserve: u8,
    /// Cache of the last folder decompressed, since files in one folder are
    /// almost always extracted together.
    cache: Option<(u16, Vec<u8>)>,
}

fn le16(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(d[p..p + 2].try_into().unwrap())
}
fn le32(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
}

fn dos_datetime(date: u16, time: u16) -> String {
    if date == 0 {
        return String::new();
    }
    let (y, mo, d) = (1980 + (date >> 9), (date >> 5) & 0xF, date & 0x1F);
    let (h, mi, s) = (time >> 11, (time >> 5) & 0x3F, (time & 0x1F) * 2);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Cabinet names use backslashes for directories, and may name paths outside
/// their own tree, which extraction must not honour.
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

pub fn is_cab(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).is_ok() && &magic == MAGIC
}

impl CabArchive {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("Cannot open CAB: {e}"))?;
        let mut head = [0u8; 36];
        file.read_exact(&mut head).map_err(|e| format!("CAB header: {e}"))?;
        if &head[0..4] != MAGIC {
            return Err("Not a cabinet file".into());
        }

        let first_file = le32(&head, 0x10) as u64;
        let folder_count = le16(&head, 0x1A) as usize;
        let file_count = le16(&head, 0x1C) as usize;
        let flags = le16(&head, 0x1E);

        // The reserve sizes shift every later structure along, so they have to be
        // read before anything else can be located.
        let mut pos = 36u64;
        let (mut folder_reserve, mut data_reserve) = (0u8, 0u8);
        if flags & FLAG_RESERVE_PRESENT != 0 {
            let mut r = [0u8; 4];
            file.read_exact(&mut r).map_err(|e| format!("CAB reserve: {e}"))?;
            let header_reserve = le16(&r, 0) as u64;
            folder_reserve = r[2];
            data_reserve = r[3];
            pos += 4 + header_reserve;
            file.seek(SeekFrom::Start(pos)).map_err(|e| format!("CAB seek: {e}"))?;
        }
        // Chained cabinets name their neighbours here; the names are skipped, but
        // their presence still moves the folder table.
        for flag in [FLAG_PREV_CABINET, FLAG_NEXT_CABINET] {
            if flags & flag != 0 {
                for _ in 0..2 {
                    pos += read_cstring_len(&mut file, pos)?;
                }
            }
        }

        // Folder table.
        let mut folders = Vec::with_capacity(folder_count);
        file.seek(SeekFrom::Start(pos)).map_err(|e| format!("CAB seek: {e}"))?;
        for _ in 0..folder_count {
            let mut b = [0u8; 8];
            file.read_exact(&mut b).map_err(|e| format!("CAB folder: {e}"))?;
            folders.push(Folder {
                data_start: le32(&b, 0) as u64,
                blocks: le16(&b, 4),
                compression: le16(&b, 6) & 0x000F,
            });
            if folder_reserve > 0 {
                file.seek(SeekFrom::Current(folder_reserve as i64))
                    .map_err(|e| format!("CAB seek: {e}"))?;
            }
        }

        // File table.
        let mut files = Vec::with_capacity(file_count);
        let mut dirs = BTreeSet::new();
        let mut at = first_file;
        for _ in 0..file_count {
            let mut b = [0u8; 16];
            file.seek(SeekFrom::Start(at)).map_err(|e| format!("CAB seek: {e}"))?;
            file.read_exact(&mut b).map_err(|e| format!("CAB file entry: {e}"))?;
            let size = le32(&b, 0);
            let folder_offset = le32(&b, 4);
            let folder = le16(&b, 8);
            let date = le16(&b, 10);
            let time = le16(&b, 12);
            let attribs = le16(&b, 14);
            at += 16;

            let (raw_name, consumed) = read_cstring(&mut file, at)?;
            at += consumed;
            let name = if attribs & ATTR_NAME_IS_UTF != 0 {
                String::from_utf8_lossy(raw_name.as_bytes()).into_owned()
            } else {
                raw_name
            };
            let Some(norm) = normalise(&name) else { continue };

            let mut parent = norm.as_str();
            while let Some(cut) = parent.rfind('/') {
                if cut == 0 {
                    break;
                }
                parent = &parent[..cut];
                dirs.insert(parent.to_string());
            }
            files.push(FileRec {
                path: norm,
                size,
                folder_offset,
                folder,
                modified: dos_datetime(date, time),
            });
        }

        if files.is_empty() {
            return Err("CAB: no files listed".into());
        }
        Ok(CabArchive { file, folders, files, dirs, data_reserve, cache: None })
    }

    /// Decompress a whole folder. Files are located by offset into this stream,
    /// so there is no way to decode one file without the bytes before it.
    fn folder_bytes(&mut self, index: u16) -> Result<&[u8], String> {
        if self.cache.as_ref().is_some_and(|(i, _)| *i == index) {
            return Ok(&self.cache.as_ref().unwrap().1);
        }
        let folder = self
            .folders
            .get(index as usize)
            .ok_or_else(|| format!("CAB: folder {index} does not exist"))?;
        let (start, blocks, compression) = (folder.data_start, folder.blocks, folder.compression);

        match compression {
            COMPRESS_NONE | COMPRESS_MSZIP => {}
            COMPRESS_LZX => return Err("CAB: LZX-compressed cabinets are not supported yet".into()),
            COMPRESS_QUANTUM => {
                return Err("CAB: Quantum-compressed cabinets are not supported yet".into())
            }
            other => return Err(format!("CAB: unknown compression type {other}")),
        }

        // Read every block header first, so the output buffer can be sized once —
        // MSZIP back-references reach into earlier blocks and need one contiguous
        // buffer.
        let mut at = start;
        let mut plan = Vec::with_capacity(blocks as usize);
        let mut total = 0usize;
        for _ in 0..blocks {
            let mut h = [0u8; 8];
            self.file.seek(SeekFrom::Start(at)).map_err(|e| format!("CAB seek: {e}"))?;
            self.file.read_exact(&mut h).map_err(|e| format!("CAB block header: {e}"))?;
            let comp = le16(&h, 4) as usize;
            let uncomp = le16(&h, 6) as usize;
            let data_at = at + 8 + self.data_reserve as u64;
            plan.push((data_at, comp, uncomp));
            total += uncomp;
            at = data_at + comp as u64;
        }

        let mut out = vec![0u8; total];
        let mut written = 0usize;
        for (data_at, comp, uncomp) in plan {
            let mut raw = vec![0u8; comp];
            self.file.seek(SeekFrom::Start(data_at)).map_err(|e| format!("CAB seek: {e}"))?;
            self.file.read_exact(&mut raw).map_err(|e| format!("CAB block: {e}"))?;

            if compression == COMPRESS_NONE {
                let end = (written + raw.len()).min(out.len());
                out[written..end].copy_from_slice(&raw[..end - written]);
                written = end;
                continue;
            }

            if raw.len() < 2 || &raw[0..2] != b"CK" {
                return Err("CAB: MSZIP block is missing its CK signature".into());
            }
            let mut state = DecompressorOxide::new();
            // The non-wrapping flag is what lets this block's back-references read
            // the previous blocks already sitting in `out`.
            let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
            let (status, _consumed, produced) = decompress(&mut state, &raw[2..], &mut out, written, flags);
            if !matches!(status, TINFLStatus::Done | TINFLStatus::HasMoreOutput) {
                return Err(format!("CAB: MSZIP block failed to inflate ({status:?})"));
            }
            if produced != uncomp {
                return Err(format!(
                    "CAB: MSZIP block produced {produced} bytes, header said {uncomp}"
                ));
            }
            written += produced;
        }

        self.cache = Some((index, out));
        Ok(&self.cache.as_ref().unwrap().1)
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

        out.extend(self.files.iter().filter_map(|f| {
            let name = child(&f.path, &prefix)?;
            Some(DiscEntry {
                name: name.to_string(),
                is_dir: false,
                lba: 0,
                size: f.size,
                size_bytes: f.size,
                modified: f.modified.clone(),
                deleted: false,
                is_xa: false,
            })
        }));

        if out.is_empty() && !trimmed.is_empty() && !self.dirs.contains(trimmed) {
            return Err(format!("Not found: {dir_path}"));
        }
        Ok(out)
    }

    fn read_file(&mut self, key: &str) -> Result<Vec<u8>, String> {
        let (folder, offset, size) = self
            .files
            .iter()
            .find(|f| f.path == key)
            .map(|f| (f.folder, f.folder_offset as usize, f.size as usize))
            .ok_or_else(|| format!("Not found: {key}"))?;
        let bytes = self.folder_bytes(folder)?;
        if offset + size > bytes.len() {
            return Err(format!(
                "CAB: {key} runs past the end of its folder ({} bytes available)",
                bytes.len().saturating_sub(offset)
            ));
        }
        Ok(bytes[offset..offset + size].to_vec())
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let key = normalise(file_path).ok_or_else(|| format!("Bad path: {file_path}"))?;
        let bytes = self.read_file(&key)?;
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
            .iter()
            .filter(|f| f.path.starts_with(&prefix))
            .map(|f| f.path.clone())
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
            let bytes = self.read_file(&key)?;
            File::create(&out)
                .map_err(|e| format!("Cannot create file: {e}"))?
                .write_all(&bytes)
                .map_err(|e| format!("Write error: {e}"))?;
        }
        Ok(())
    }
}

fn read_cstring(file: &mut File, at: u64) -> Result<(String, u64), String> {
    file.seek(SeekFrom::Start(at)).map_err(|e| format!("CAB seek: {e}"))?;
    let mut name = Vec::new();
    let mut b = [0u8; 1];
    loop {
        let n = file.read(&mut b).map_err(|e| format!("CAB read: {e}"))?;
        if n == 0 {
            return Err("CAB: name is not terminated".into());
        }
        if b[0] == 0 {
            break;
        }
        name.push(b[0]);
        if name.len() > 1024 {
            return Err("CAB: name is implausibly long".into());
        }
    }
    let consumed = name.len() as u64 + 1;
    Ok((String::from_utf8_lossy(&name).into_owned(), consumed))
}

fn read_cstring_len(file: &mut File, at: u64) -> Result<u64, String> {
    read_cstring(file, at).map(|(_, n)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_traversal_in_cabinet_names() {
        assert_eq!(normalise("dir\\file.txt").as_deref(), Some("/dir/file.txt"));
        assert_eq!(normalise("file.txt").as_deref(), Some("/file.txt"));
        assert_eq!(normalise("..\\..\\windows\\system32\\evil.dll"), None);
        assert_eq!(normalise("a/../../b"), None);
    }

    #[test]
    fn decodes_dos_timestamps() {
        let date = ((2010 - 1980) << 9) | (7 << 5) | 4;
        let time = (9 << 11) | (5 << 5) | 15;
        assert_eq!(dos_datetime(date, time), "2010-07-04 09:05:30");
        assert_eq!(dos_datetime(0, 0), "");
    }
}
