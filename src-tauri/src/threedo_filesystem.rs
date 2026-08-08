// 3DO Interactive Multiplayer (OperaFS) filesystem reader.
//
// The 3DO CD-ROM uses the Opera filesystem identified by a 7-byte magic at
// the first data sector: {0x01, 0x5A×5, 0x01}.
//
// Volume header (LBA 0, all fields big-endian) — layout confirmed by real disc probe:
//   bytes 0-6:    magic {0x01, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x01}
//   byte  7:      flags
//   bytes 8-39:   comment (32 bytes, null-padded)
//   bytes 40-71:  volume label (32 bytes, null-padded)
//   bytes 72-75:  unique id (BE)
//   bytes 76-79:  block_size (BE, always 2048)
//   bytes 80-83:  block_count (BE)
//   bytes 84-87:  root directory flags/id (BE)
//   bytes 88-91:  root directory first LBA (BE)
//   bytes 92-95:  root directory byte count (BE)
//   bytes 96-99:  root directory block count (BE)
//
// Directory block (2048 bytes):
//   bytes 0-3:   next block (BE, 0xFFFFFFFF = last) — an index within this
//                directory's own extent, not an absolute LBA, so the block to
//                read next is the directory's first block plus this value
//   bytes 4-7:   prev block, same numbering
//   bytes 8-11:  flags (BE)
//   bytes 12-15: first free byte offset (BE) — end of valid entry data
//   bytes 16-19: first entry offset (BE) — where the entries start, normally 20
//   bytes 20+:   directory entries, variable length
//
// Directory entry (68 bytes, plus 4 per avatar):
//   bytes 0-3:   flags (BE)
//   bytes 4-7:   unique id (BE)
//   bytes 8-11:  type tag (BE) — TYPE_DIR='Cat ', TYPE_FILE='Lvl '
//   bytes 12-15: block size (BE)
//   bytes 16-19: byte count (BE) — exact file size
//   bytes 20-23: block count (BE) — size in 2048-byte blocks
//   bytes 24-27: burst (BE)
//   bytes 28-31: gap (BE)
//   bytes 32-63: name (32 bytes, null-terminated ASCII)
//   bytes 64-67: last avatar index (BE)
//   bytes 68+:   avatar[0..=last] (BE) — starting LBA of each copy
//
// An entry is therefore 68 + 4 * (last_avatar + 1) bytes, not a fixed 72. One
// copy is the common case, but a disc label, rom_tags and every directory are
// routinely written two or three times over, and assuming 72 misaligns every
// entry after the first such record.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use crate::DiscEntry;

const OPERA_MAGIC: [u8; 7] = [0x01, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x01];

// Root dir first LBA is at volume header offset 100 (confirmed by disc probe).
// Offset 88 holds the root dir unique ID, not the LBA.
const VOL_ROOT_LBA_OFF: usize = 100;

const DIR_HDR_SIZE: usize = 20;
/// Fixed part of an entry, before the avatar list.
const DIR_ENTRY_FIXED: usize = 68;
/// Cap on avatars, so a corrupt record cannot run the walk off the block.
const MAX_AVATARS: usize = 64;
const ENTRY_TYPE_OFF: usize = 8;
const ENTRY_BYTECOUNT_OFF: usize = 16;
const ENTRY_NAME_OFF: usize = 32;
const ENTRY_NAME_LEN: usize = 32;
const ENTRY_LAST_AVATAR_OFF: usize = 64;
const ENTRY_AVATAR0_OFF: usize = 68;

/// Length of the entry starting at `buf`, including its avatar list.
fn entry_size(buf: &[u8]) -> Option<usize> {
    if buf.len() < DIR_ENTRY_FIXED + 4 { return None; }
    let last = be_u32(buf, ENTRY_LAST_AVATAR_OFF) as usize;
    if last >= MAX_AVATARS { return None; }
    Some(DIR_ENTRY_FIXED + 4 * (last + 1))
}

// Type tags vary by disc mastering tool:
//   'Cat ' (0x43617420) and '*dir' (0x2A646972) both mark directories
//   'Lvl ' (0x4C766C20) and '    ' (0x20202020) both mark files
// Flags bit 2 (0x4) is set for directories; bit 1 (0x2) for files — used as fallback.

fn be_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

pub fn default_stride(user_data_offset: u64) -> u64 {
    if user_data_offset > 0 { 2352 } else { 2048 }
}

fn read_sector<F: Read + Seek>(
    file: &mut F,
    track_offset: u64,
    stride: u64,
    user_data_offset: u64,
    lba: u64,
) -> Option<[u8; 2048]> {
    let pos = track_offset + lba * stride + user_data_offset;
    file.seek(SeekFrom::Start(pos)).ok()?;
    let mut buf = [0u8; 2048];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn trim_null(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

// ── Detection ─────────────────────────────────────────────────────────────────

pub fn is_threedo_disc(path: &Path, track_offset: u64, user_data_offset: u64) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let stride = default_stride(user_data_offset);
    let Some(s) = read_sector(&mut f, track_offset, stride, user_data_offset, 0) else { return false };
    s[0..7] == OPERA_MAGIC
}

pub fn is_threedo_reader<F: Read + Seek>(
    reader: &mut F,
    track_byte_start: u64,
    user_data_offset: u64,
    stride: u64,
) -> bool {
    let Some(s) = read_sector(reader, track_byte_start, stride, user_data_offset, 0) else { return false };
    s[0..7] == OPERA_MAGIC
}

// ── Internal entry type ───────────────────────────────────────────────────────

#[derive(PartialEq)]
enum EntryKind { File, Directory }

struct DirEntry {
    name: String,
    kind: EntryKind,
    lba: u32,
    byte_count: u32,
    #[allow(dead_code)]
    block_count: u32,
}

fn parse_entry(buf: &[u8]) -> Option<DirEntry> {
    if buf.len() < DIR_ENTRY_FIXED + 4 { return None; }
    let type_tag = be_u32(buf, ENTRY_TYPE_OFF);
    let flags = be_u32(buf, 0);
    let kind = match type_tag {
        0x43617420 | 0x2A646972 => EntryKind::Directory, // 'Cat ' or '*dir'
        0x4C766C20 | 0x20202020 => EntryKind::File,      // 'Lvl ' or '    '
        0x2A6C626C => EntryKind::File,                   // '*lbl' — the disc label
        _ => {
            // The low three bits carry the type: 7 is a directory, 2 a plain file
            // and 6 a special one such as the label. Testing bit 2 alone claims
            // every special file is a directory.
            match flags & 7 {
                7 => EntryKind::Directory,
                2 | 6 => EntryKind::File,
                _ => return None,
            }
        }
    };
    let name = trim_null(&buf[ENTRY_NAME_OFF..ENTRY_NAME_OFF + ENTRY_NAME_LEN]);
    if name.is_empty() { return None; }
    let byte_count = be_u32(buf, ENTRY_BYTECOUNT_OFF);
    let block_count = be_u32(buf, 20);
    let lba = be_u32(buf, ENTRY_AVATAR0_OFF);
    Some(DirEntry { name, kind, lba, byte_count, block_count })
}

// ── Filesystem ────────────────────────────────────────────────────────────────

pub struct ThreeDOFs<F: Read + Seek> {
    file: F,
    track_offset: u64,
    user_data_offset: u64,
    stride: u64,
    root_lba: u64,
}

impl<F: Read + Seek> ThreeDOFs<F> {
    pub fn new(mut file: F, track_offset: u64, user_data_offset: u64, stride: u64) -> Result<Self, String> {
        let s = read_sector(&mut file, track_offset, stride, user_data_offset, 0)
            .ok_or_else(|| "Cannot read 3DO volume header".to_string())?;
        if s[0..7] != OPERA_MAGIC {
            return Err("Not a 3DO OperaFS disc".to_string());
        }
        let root_lba = be_u32(&s, VOL_ROOT_LBA_OFF) as u64;
        Ok(ThreeDOFs { file, track_offset, user_data_offset, stride, root_lba })
    }

    fn read_sector(&mut self, lba: u64) -> Option<[u8; 2048]> {
        read_sector(&mut self.file, self.track_offset, self.stride, self.user_data_offset, lba)
    }

    fn read_dir_at(&mut self, start_lba: u64) -> Vec<DirEntry> {
        let mut entries = Vec::new();
        let mut lba = start_lba;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..256 { // safety cap to avoid infinite loops on corrupt images
            if !seen.insert(lba) { break; }
            let block = match self.read_sector(lba) {
                Some(b) => b,
                None => break,
            };
            let next = be_u32(&block, 0);
            // first_free (offset 12) marks the end of valid entry data in this block.
            // Clamping iteration to it prevents reading garbage past the used portion.
            let first_free = (be_u32(&block, 12) as usize).min(2048);
            // Entries start where the header says, not at a fixed 20 — and they
            // run until first_free rather than for a counted number, because the
            // field at offset 16 is that starting offset, not a count.
            let mut off = (be_u32(&block, 16) as usize).clamp(DIR_HDR_SIZE, 2048);
            while off + DIR_ENTRY_FIXED + 4 <= first_free {
                let Some(size) = entry_size(&block[off..]) else { break };
                if off + size > first_free { break; }
                if let Some(e) = parse_entry(&block[off..off + size]) {
                    entries.push(e);
                }
                off += size;
            }
            // A directory longer than one block chains on by index within its own
            // extent: 1 is the block after its first, not absolute LBA 1. Reading
            // it as an LBA lands on the disc's second sector and the walk stops,
            // truncating every directory that spans more than one block.
            if next == 0xFFFF_FFFF || next == 0 { break; }
            lba = start_lba + next as u64;
        }
        entries
    }

    fn resolve_dir(&mut self, path: &str) -> Result<u64, String> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut lba = self.root_lba;
        for part in parts {
            let entries = self.read_dir_at(lba);
            let dir = entries.into_iter()
                .find(|e| e.kind == EntryKind::Directory && e.name == part)
                .ok_or_else(|| format!("Directory not found: {part}"))?;
            lba = dir.lba as u64;
        }
        Ok(lba)
    }

    pub fn list_directory(&mut self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let lba = self.resolve_dir(dir_path)?;
        let entries = self.read_dir_at(lba);
        Ok(entries.into_iter().map(|e| {
            let is_dir = e.kind == EntryKind::Directory;
            DiscEntry {
                is_xa: false,
                deleted: false,
                name: e.name,
                is_dir,
                lba: e.lba,
                size: if is_dir { 0 } else { e.byte_count },
                size_bytes: e.byte_count,
                modified: String::new(),
            }
        }).collect())
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let (dir, name) = split_path(file_path);
        let dir_lba = self.resolve_dir(dir)?;
        let entries = self.read_dir_at(dir_lba);
        let entry = entries.into_iter()
            .find(|e| e.kind == EntryKind::File && e.name == name)
            .ok_or_else(|| format!("File not found: {file_path}"))?;
        let mut out = File::create(dest_path)
            .map_err(|e| format!("Cannot create output: {e}"))?;
        self.write_blocks(entry.lba as u64, entry.byte_count as u64, &mut out)
    }

    fn write_blocks(&mut self, start_lba: u64, byte_count: u64, out: &mut File) -> Result<(), String> {
        let mut remaining = byte_count;
        let mut lba = start_lba;
        while remaining > 0 {
            let block = self.read_sector(lba)
                .ok_or_else(|| format!("Cannot read block {lba}"))?;
            let n = remaining.min(2048) as usize;
            out.write_all(&block[..n])
                .map_err(|e| format!("Write error: {e}"))?;
            remaining = remaining.saturating_sub(2048);
            lba += 1;
        }
        Ok(())
    }
}

fn split_path(path: &str) -> (&str, &str) {
    let path = path.trim_start_matches('/');
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One directory block holding entries with differing avatar counts, which is
    /// the shape that used to break the walk.
    fn dir_block(entries: &[(&str, u32, u32, &[u32])]) -> [u8; 2048] {
        let mut b = [0u8; 2048];
        b[0..4].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        b[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        let mut off = DIR_HDR_SIZE;
        for (name, flags, type_tag, avatars) in entries {
            b[off..off + 4].copy_from_slice(&flags.to_be_bytes());
            b[off + 8..off + 12].copy_from_slice(&type_tag.to_be_bytes());
            b[off + 16..off + 20].copy_from_slice(&64u32.to_be_bytes()); // byte count
            let n = name.as_bytes();
            b[off + ENTRY_NAME_OFF..off + ENTRY_NAME_OFF + n.len()].copy_from_slice(n);
            b[off + ENTRY_LAST_AVATAR_OFF..off + ENTRY_LAST_AVATAR_OFF + 4]
                .copy_from_slice(&((avatars.len() - 1) as u32).to_be_bytes());
            for (i, a) in avatars.iter().enumerate() {
                let at = off + ENTRY_AVATAR0_OFF + i * 4;
                b[at..at + 4].copy_from_slice(&a.to_be_bytes());
            }
            off += DIR_ENTRY_FIXED + 4 * avatars.len();
        }
        b[12..16].copy_from_slice(&(off as u32).to_be_bytes());   // first free
        b[16..20].copy_from_slice(&(DIR_HDR_SIZE as u32).to_be_bytes()); // first entry
        b
    }

    fn walk(block: &[u8; 2048]) -> Vec<DirEntry> {
        let first_free = (be_u32(block, 12) as usize).min(2048);
        let mut off = (be_u32(block, 16) as usize).clamp(DIR_HDR_SIZE, 2048);
        let mut out = Vec::new();
        while off + DIR_ENTRY_FIXED + 4 <= first_free {
            let Some(size) = entry_size(&block[off..]) else { break };
            if off + size > first_free { break; }
            if let Some(e) = parse_entry(&block[off..off + size]) { out.push(e); }
            off += size;
        }
        out
    }

    // A label written twice, then ordinary files, then directories written three
    // times over — the layout of a real 3DO disc. Assuming a fixed 72-byte entry
    // lands mid-record after the first multi-avatar entry and the walk stops.
    #[test]
    fn entries_with_several_avatars_do_not_derail_the_walk() {
        let block = dir_block(&[
            ("Disc label", 0x0006, 0x2A6C626C, &[0, 225]),
            ("AppStartup", 0x0002, 0x2020_2020, &[63]),
            ("rom_tags", 0x0002, 0x2020_2020, &[1, 226]),
            ("System", 0xC000_0007, 0x2A64_6972, &[329742, 329806, 329870]),
        ]);
        let got = walk(&block);
        let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Disc label", "AppStartup", "rom_tags", "System"]);
        assert_eq!(got[0].lba, 0);
        assert_eq!(got[2].lba, 1, "rom_tags reads its first avatar");
        assert_eq!(got[3].lba, 329742);
    }

    // The label is a special file, not a directory: its flags are 6, and testing
    // bit 2 on its own wrongly claims it.
    #[test]
    fn the_disc_label_is_a_file() {
        let block = dir_block(&[("Disc label", 0x0006, 0x2A6C626C, &[0, 225])]);
        assert!(matches!(walk(&block)[0].kind, EntryKind::File));

        // Unknown type tag, so the flags decide.
        let block = dir_block(&[("odd", 0x0006, 0x1234_5678, &[7])]);
        assert!(matches!(walk(&block)[0].kind, EntryKind::File));
        let block = dir_block(&[("adir", 0x0007, 0x1234_5678, &[7])]);
        assert!(matches!(walk(&block)[0].kind, EntryKind::Directory));
    }

    #[test]
    fn an_absurd_avatar_count_stops_the_walk() {
        let mut block = dir_block(&[("x", 0x0002, 0x2020_2020, &[5])]);
        block[DIR_HDR_SIZE + ENTRY_LAST_AVATAR_OFF..DIR_HDR_SIZE + ENTRY_LAST_AVATAR_OFF + 4]
            .copy_from_slice(&9_999u32.to_be_bytes());
        assert!(walk(&block).is_empty());
    }
}
