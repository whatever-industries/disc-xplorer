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

/// Result of checking a disc's RSA signature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignatureStatus {
    /// The signature verifies against the retail key: a console would accept it.
    Signed,
    /// A recognised placeholder — the signing step never ran.
    Unsigned,
    /// A signature is present but does not verify. A disc modified after
    /// signing, or signed with a key that is not the retail one.
    Invalid,
    /// Nothing to check.
    Absent,
}

impl SignatureStatus {
    pub fn label(self) -> &'static str {
        match self {
            SignatureStatus::Signed => "Signed",
            SignatureStatus::Unsigned => "Unsigned",
            SignatureStatus::Invalid => "Invalid signature",
            SignatureStatus::Absent => "",
        }
    }
}

// ── Signature verification ────────────────────────────────────────────────────
//
// A 3DO disc is signed by RSA-512 over an MD5 digest of its disc label, its ROM
// tag table, and the boot code the NEWKNEWNEWGNUBOOT tag points at. The console
// checks this before it will run anything, which is what "signed" means for a
// 3DO disc.
//
// Verification recovers the signature with the public modulus and compares it
// against the PKCS#1 v1.5 encoding of the digest:
//
//   recovered = signature ^ 65537 mod n
//   expected  = 0x1f || 0xff * 27 || 0x00 || <DigestInfo for MD5> || digest
//
// Keys and layout come from 3dt (ISC, Copyright (c) 2025 Antonio SJ Musumeci),
// src/tdo_keys.cpp and src/subcmd_verify.cpp.

/// Retail application key modulus. This is the one that signs the disc label,
/// ROM tags and boot code together.
const RETAIL_APP_MODULUS: &str = "BC0B199086C7F26CBC9D50F404944DB4789FCBFCF7AD8DBC2120898ABEAAF311EEA20229035608841FA41073ABBD5D37500C60B53BFB46605740381B72C9DB71";
/// Retail system key, used for other signatures on the disc; tried as a fallback.
const RETAIL_3DO_MODULUS: &str = "B19462B00D8D6E1EC909AB385E06FE034BFD282E9FFDC584838C15F12593DD1E3A8B5626F1B9D0ED0C384EF6C5D14512BD72DDB85B44080E0472C03D0AFC4C97";
const PUBLIC_EXPONENT: u32 = 65537;
const SIGNATURE_SIZE: usize = 64;
const ROMTAG_SIZE: usize = 32;
/// Opera's disc label is a fixed 132-byte structure, whatever the catalog says
/// the "Disc label" file's length is.
const DISC_LABEL_SIZE: usize = 132;
/// ROM tag type marking the boot code covered by the signature.
const RSA_NEWKNEWNEWGNUBOOT: u8 = 0x0D;
/// Guard against a table with no terminator.
const MAX_ROMTAGS: usize = 64;

/// PKCS#1 v1.5 padding with the MD5 DigestInfo, as a hex string ready for the
/// digest to be appended.
const PKCS1_MD5_PREFIX: &str =
    "1ffffffffffffffffffffffffffffffffffffffffffffffffffffff003020300c06082a864886f70d020505000410";

/// "iamaduck" repeated — the placeholder an unsigned prerelease disc carries in
/// place of a signature.
const IAMADUCK: &[u8; 8] = b"iamaduck";

fn is_placeholder(sig: &[u8]) -> bool {
    sig.iter().all(|&b| b == 0)
        || sig
            .iter()
            .enumerate()
            .all(|(i, &b)| b == IAMADUCK[i % IAMADUCK.len()])
}

fn verify_signature(digest: &[u8; 16], sig: &[u8]) -> bool {
    use num_bigint::BigUint;

    let expected = {
        let mut hex = String::with_capacity(PKCS1_MD5_PREFIX.len() + 32);
        hex.push_str(PKCS1_MD5_PREFIX);
        for b in digest {
            hex.push_str(&format!("{b:02x}"));
        }
        match BigUint::parse_bytes(hex.as_bytes(), 16) {
            Some(v) => v,
            None => return false,
        }
    };

    let signature = BigUint::from_bytes_be(sig);
    let exponent = BigUint::from(PUBLIC_EXPONENT);
    [RETAIL_APP_MODULUS, RETAIL_3DO_MODULUS].iter().any(|m| {
        BigUint::parse_bytes(m.as_bytes(), 16)
            .map(|n| signature.modpow(&exponent, &n) == expected)
            .unwrap_or(false)
    })
}

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

    /// Read `len` bytes starting at a block.
    fn read_bytes(&mut self, block: u64, len: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut b = block;
        while out.len() < len {
            let sector = self.read_sector(b)?;
            let take = (len - out.len()).min(sector.len());
            out.extend_from_slice(&sector[..take]);
            b += 1;
        }
        Some(out)
    }

    /// Length of the ROM tag table, found by scanning for its terminator.
    ///
    /// This cannot be taken from the `rom_tags` catalog entry: the table ends at
    /// the first tag whose subsystem type is zero, and that terminator counts
    /// towards the length, so the real table is longer than the file the catalog
    /// describes. Using the catalog's length puts the signature in the wrong
    /// place and nothing verifies.
    fn romtags_size(&mut self, romtags_block: u64) -> Option<usize> {
        let table = self.read_bytes(romtags_block, MAX_ROMTAGS * ROMTAG_SIZE)?;
        for i in 0..MAX_ROMTAGS {
            if table[i * ROMTAG_SIZE] == 0 {
                return Some((i + 1) * ROMTAG_SIZE);
            }
        }
        None
    }

    /// Whether this disc's RSA signature is valid.
    ///
    /// The digest covers the disc label, the ROM tag table, and the boot code
    /// the NEWKNEWNEWGNUBOOT tag points at, in that order.
    pub fn signature_status(&mut self) -> SignatureStatus {
        // The label is a fixed-size structure at block 0, and the tags follow it.
        let romtags_block = DISC_LABEL_SIZE.div_ceil(2048) as u64;

        let Some(romtags_size) = self.romtags_size(romtags_block) else {
            return SignatureStatus::Absent;
        };
        let Some(romtags) = self.read_bytes(romtags_block, romtags_size) else {
            return SignatureStatus::Absent;
        };

        // The signature sits immediately after the table.
        let Some(tail) = self.read_bytes(
            romtags_block + (romtags_size / 2048) as u64,
            (romtags_size % 2048) + SIGNATURE_SIZE,
        ) else {
            return SignatureStatus::Absent;
        };
        let sig = &tail[romtags_size % 2048..];

        if is_placeholder(sig) {
            return SignatureStatus::Unsigned;
        }

        // The boot code the signature also covers. Its tag records a block
        // offset from the start of the tag table, not an absolute LBA.
        let mut boot = None;
        for i in 0..(romtags_size / ROMTAG_SIZE) {
            let tag = &romtags[i * ROMTAG_SIZE..];
            if tag[1] == RSA_NEWKNEWNEWGNUBOOT {
                boot = Some((be_u32(tag, 8) as u64, be_u32(tag, 12) as usize));
                break;
            }
        }
        let Some((boot_offset, boot_size)) = boot else {
            return SignatureStatus::Invalid;
        };

        let (Some(label), Some(boot_code)) = (
            self.read_bytes(0, DISC_LABEL_SIZE),
            self.read_bytes(romtags_block + boot_offset, boot_size),
        ) else {
            return SignatureStatus::Absent;
        };

        use md5::{Digest, Md5};
        let mut hasher = Md5::new();
        hasher.update(&label);
        hasher.update(&romtags);
        hasher.update(&boot_code);
        let digest: [u8; 16] = hasher.finalize().into();

        if verify_signature(&digest, sig) {
            SignatureStatus::Signed
        } else {
            SignatureStatus::Invalid
        }
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

    // A real signature and its digest, lifted from a retail disc (3D Atlas). The
    // point of keeping them is the negative cases: a verifier that accepts
    // everything would pass the positive check just as happily.
    const REAL_DIGEST: [u8; 16] = [
        0xcf, 0xca, 0x43, 0x78, 0x31, 0xe7, 0xfb, 0x06,
        0x40, 0x21, 0xa0, 0x77, 0x22, 0xc9, 0xc7, 0x4f,
    ];
    const REAL_SIG: [u8; 64] = [
        0x7a, 0xc4, 0xa2, 0xfb, 0x6a, 0x0a, 0x4d, 0x59,
        0x23, 0x31, 0xe8, 0xf7, 0x3f, 0x5b, 0xef, 0x05,
        0x4e, 0xc4, 0xeb, 0xdb, 0x8d, 0xa8, 0x7d, 0x02,
        0xa3, 0xe1, 0x46, 0x38, 0xfb, 0xe9, 0xec, 0x84,
        0x6e, 0x53, 0x81, 0x3c, 0x31, 0xe0, 0xd1, 0x96,
        0x9b, 0x00, 0x4f, 0x2d, 0x02, 0x1a, 0x26, 0xc1,
        0x78, 0x26, 0x30, 0xbf, 0x82, 0x02, 0xbc, 0x79,
        0x3a, 0x36, 0x95, 0xb2, 0xf4, 0x60, 0x20, 0x84,
    ];

    #[test]
    fn a_real_retail_signature_verifies() {
        assert!(verify_signature(&REAL_DIGEST, &REAL_SIG));
    }

    #[test]
    fn verification_rejects_what_it_should() {
        // One flipped bit in the data being attested.
        let mut digest = REAL_DIGEST;
        digest[0] ^= 1;
        assert!(!verify_signature(&digest, &REAL_SIG), "a changed digest must fail");

        // One flipped bit in the signature itself.
        let mut sig = REAL_SIG;
        sig[0] ^= 1;
        assert!(!verify_signature(&REAL_DIGEST, &sig), "a changed signature must fail");

        // And the degenerate inputs a broken verifier tends to accept.
        assert!(!verify_signature(&REAL_DIGEST, &[0u8; 64]));
        assert!(!verify_signature(&REAL_DIGEST, &[0xFFu8; 64]));
        assert!(!verify_signature(&[0u8; 16], &REAL_SIG));
    }

    #[test]
    fn placeholders_are_recognised() {
        assert!(is_placeholder(&[0u8; 64]), "zero placeholder");
        let duck: Vec<u8> = (0..64).map(|i| b"iamaduck"[i % 8]).collect();
        assert!(is_placeholder(&duck), "iamaduck placeholder");
        assert!(!is_placeholder(&REAL_SIG), "a real signature is not a placeholder");
    }

    #[test]
    fn an_absurd_avatar_count_stops_the_walk() {
        let mut block = dir_block(&[("x", 0x0002, 0x2020_2020, &[5])]);
        block[DIR_HDR_SIZE + ENTRY_LAST_AVATAR_OFF..DIR_HDR_SIZE + ENTRY_LAST_AVATAR_OFF + 4]
            .copy_from_slice(&9_999u32.to_be_bytes());
        assert!(walk(&block).is_empty());
    }
}
