// CD-i Green Book filesystem reader (Chapter III of Full Functional Specification)
// All multi-byte fields are big-endian only (unlike ISO 9660 which uses both-endian).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::OnceLock;
use crate::DiscEntry;

const SECTOR_SIZE: u64 = 2352;
const BLOCK_SIZE: usize = 2048;

// ECMA-130 scramble table: 15-bit LFSR (X^15 + X + 1), initial state 1, LSB-first per byte.
// Covers bytes 12-2351 of each raw sector (2340 bytes). Sectors are individually scrambled
// (LFSR resets to 1 at the start of each sector's scrambled region).
static SCRAMBLE_TABLE: OnceLock<[u8; 2340]> = OnceLock::new();

pub fn scramble_table() -> &'static [u8; 2340] {
    SCRAMBLE_TABLE.get_or_init(|| {
        let mut state: u16 = 1;
        let mut table = [0u8; 2340];
        for byte in table.iter_mut() {
            let mut val = 0u8;
            for bit in 0..8 {
                let out = (state & 1) as u8;
                val |= out << bit;
                let feedback = (state & 1) ^ ((state >> 1) & 1);
                state = ((state >> 1) | (feedback << 14)) & 0x7FFF;
            }
            *byte = val;
        }
        table
    })
}

pub struct CdiFs {
    file: File,
    track_offset: u64,
    user_data_offset: u64,
    lba_offset: u64,
    descramble: bool,
    pub root_lba: u32,
    pub volume_id: String,
}

impl CdiFs {
    pub fn new(
        mut file: File,
        track_offset: u64,
        user_data_offset: u64,
        lba_offset: u64,
        descramble: bool,
    ) -> Result<Self, String> {
        let buf = read_block(&mut file, track_offset, user_data_offset, lba_offset, 16, descramble)?;

        // CD-i disc-label records normally begin with an 8-byte descriptor-LBN
        // field.  Some images omit it, so accept both layouts.  All remaining
        // offsets in the record are relative to the type byte.
        let record_offset = cdi_record_offset(&buf).ok_or_else(|| {
            format!(
                "Not a CD-i volume descriptor (type={}, id={})",
                buf[0],
                std::str::from_utf8(&buf[1..6]).unwrap_or("?")
            )
        })?;

        // Volume identifier: bytes 40-71 relative to the record type,
        // space-padded.
        let volume_id = std::str::from_utf8(&buf[record_offset + 40..record_offset + 72])
            .unwrap_or("")
            .trim_end()
            .to_string();

        // Address of Path Table, 4 bytes big-endian.  A full Green Book record
        // includes a descriptor-LBN prefix and has its path-table pointer at
        // bytes 156-159 relative to the type; compact records use 148-151.
        let path_table_offset = if record_offset == 8 { 156 } else { 148 };
        let path_table_lba = u32::from_be_bytes([
            buf[record_offset + path_table_offset],
            buf[record_offset + path_table_offset + 1],
            buf[record_offset + path_table_offset + 2],
            buf[record_offset + path_table_offset + 3],
        ]);

        // Read the path table; the first entry describes the root directory.
        // Green Book tables start directly with a 4-byte directory LBN. The
        // compact legacy layout retains the ISO-style two-byte prefix.
        let pt = read_block(&mut file, track_offset, user_data_offset, lba_offset, path_table_lba as u64, descramble)?;
        let root_lba = if record_offset == 8 {
            u32::from_be_bytes([pt[0], pt[1], pt[2], pt[3]])
        } else {
            u32::from_be_bytes([pt[2], pt[3], pt[4], pt[5]])
        };

        Ok(Self {
            file,
            track_offset,
            user_data_offset,
            lba_offset,
            descramble,
            root_lba,
            volume_id,
        })
    }

    pub fn list_directory(&mut self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let lba = self.resolve_path(dir_path)?;
        let buf = read_block(
            &mut self.file,
            self.track_offset,
            self.user_data_offset,
            self.lba_offset,
            lba as u64,
            self.descramble,
        )?;
        let mut entries = parse_dir_records(&buf)?;
        // Report streaming (real-time / Form 2) files at their on-disc size
        // (2336 bytes/sector) so the listing matches what extraction produces.
        if self.user_data_offset == 24 {
            for e in entries.iter_mut().filter(|e| !e.is_dir) {
                if let Ok(p) = read_raw_xa_sector(
                    &mut self.file, self.track_offset, self.lba_offset, e.lba as u64, self.descramble,
                ) {
                    if (p[2] & 0x60) != 0 {
                        let sectors = (e.size_bytes as u64).div_ceil(BLOCK_SIZE as u64);
                        let sz = (sectors * 2336).min(u32::MAX as u64) as u32;
                        e.size = sz;
                        e.size_bytes = sz;
                    }
                }
            }
        }
        Ok(entries)
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let (lba, size) = self.find_file(file_path)?;
        let mut dest = File::create(dest_path)
            .map_err(|e| format!("Cannot create destination: {e}"))?;

        let num_blocks = (size as u64 + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64;

        // CD-i streaming files (audio/video) use Mode 2 sectors whose payload is
        // 2336 bytes — extracting only the 2048 logical bytes truncates them.
        // Classify from the first sector's subheader submode: bit 6 (0x40) =
        // real-time, bit 5 (0x20) = Form 2 — either marks a streaming file (an
        // interleaved stream is real-time throughout even where individual sectors
        // are Form 1). On a raw Mode 2 source, extract the whole file at 2336
        // bytes/sector. A 2048-byte source has no subheader → logical path below.
        let streaming = self.user_data_offset == 24
            && num_blocks > 0
            && read_raw_xa_sector(
                &mut self.file, self.track_offset, self.lba_offset, lba as u64, self.descramble,
            )
            .map(|p| (p[2] & 0x60) != 0)
            .unwrap_or(false);
        if streaming {
            for i in 0..num_blocks {
                let payload = read_raw_xa_sector(
                    &mut self.file, self.track_offset, self.lba_offset, lba as u64 + i, self.descramble,
                )?;
                dest.write_all(&payload)
                    .map_err(|e| format!("Write error: {e}"))?;
            }
            return Ok(());
        }

        let mut remaining = size as u64;
        for i in 0..num_blocks {
            let buf = read_block(
                &mut self.file,
                self.track_offset,
                self.user_data_offset,
                self.lba_offset,
                lba as u64 + i,
                self.descramble,
            )?;
            let to_write = remaining.min(BLOCK_SIZE as u64) as usize;
            dest.write_all(&buf[..to_write])
                .map_err(|e| format!("Write error: {e}"))?;
            remaining -= to_write as u64;
        }

        Ok(())
    }

    fn resolve_path(&mut self, path: &str) -> Result<u32, String> {
        let mut lba = self.root_lba;
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            let buf = read_block(
                &mut self.file,
                self.track_offset,
                self.user_data_offset,
                self.lba_offset,
                lba as u64,
                self.descramble,
            )?;
            let entries = parse_dir_records(&buf)?;
            lba = entries
                .iter()
                .find(|e| e.is_dir && e.name.eq_ignore_ascii_case(segment))
                .map(|e| e.lba)
                .ok_or_else(|| format!("Directory not found: '{segment}'"))?;
        }
        Ok(lba)
    }

    fn find_file(&mut self, path: &str) -> Result<(u32, u32), String> {
        let (dir_path, file_name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        let dir_lba = if dir_path.is_empty() || dir_path == "/" {
            self.root_lba
        } else {
            self.resolve_path(dir_path)?
        };
        let buf = read_block(
            &mut self.file,
            self.track_offset,
            self.user_data_offset,
            self.lba_offset,
            dir_lba as u64,
            self.descramble,
        )?;
        parse_dir_records(&buf)?
            .iter()
            .find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(file_name))
            .map(|e| (e.lba, e.size_bytes))
            .ok_or_else(|| format!("File not found: '{file_name}'"))
    }
}

// Return the position of the CD-i disc-label record's type byte. Green Book
// descriptors include an 8-byte descriptor-LBN prefix; accepting the compact
// form too keeps support for images made by older tools.
fn cdi_record_offset(buf: &[u8]) -> Option<usize> {
    let has_id = |offset: usize| {
        buf.len() >= offset + 6
            && buf[offset] == 1
            && (&buf[offset + 1..offset + 6] == b"CD-I "
                || &buf[offset + 1..offset + 6] == b"CD_I ")
    };
    if has_id(8) {
        Some(8)
    } else if has_id(0) {
        Some(0)
    } else {
        None
    }
}

fn read_block(
    file: &mut File,
    track_offset: u64,
    user_data_offset: u64,
    lba_offset: u64,
    lba: u64,
    descramble: bool,
) -> Result<[u8; BLOCK_SIZE], String> {
    let adjusted = if lba >= lba_offset { lba - lba_offset } else { lba };
    if descramble {
        // Read the full 2352-byte raw sector, descramble bytes 12-2351, then extract user data.
        let pos = track_offset + adjusted * SECTOR_SIZE;
        file.seek(SeekFrom::Start(pos))
            .map_err(|e| format!("Seek error at LBA {lba}: {e}"))?;
        let mut sector = [0u8; 2352];
        file.read_exact(&mut sector)
            .map_err(|e| format!("Read error at LBA {lba}: {e}"))?;
        let table = scramble_table();
        for i in 12..2352usize {
            sector[i] ^= table[i - 12];
        }
        let start = user_data_offset as usize;
        let mut buf = [0u8; BLOCK_SIZE];
        buf.copy_from_slice(&sector[start..start + BLOCK_SIZE]);
        Ok(buf)
    } else {
        let pos = track_offset + adjusted * SECTOR_SIZE + user_data_offset;
        file.seek(SeekFrom::Start(pos))
            .map_err(|e| format!("Seek error at LBA {lba}: {e}"))?;
        let mut buf = [0u8; BLOCK_SIZE];
        file.read_exact(&mut buf)
            .map_err(|e| format!("Read error at LBA {lba}: {e}"))?;
        Ok(buf)
    }
}

// Raw CD-ROM XA payload (2336 bytes: subheader + data + EDC, i.e. the sector
// minus its 16-byte sync+header) for one sector. CD-i real-time files use Mode 2
// Form 2 sectors whose payload is 2336 bytes, not the 2048 logical bytes — used
// to extract them without truncation. Only valid on raw 2352-byte sources.
fn read_raw_xa_sector(
    file: &mut File,
    track_offset: u64,
    lba_offset: u64,
    lba: u64,
    descramble: bool,
) -> Result<[u8; 2336], String> {
    let adjusted = if lba >= lba_offset { lba - lba_offset } else { lba };
    let pos = track_offset + adjusted * SECTOR_SIZE;
    file.seek(SeekFrom::Start(pos))
        .map_err(|e| format!("Seek error at LBA {lba}: {e}"))?;
    let mut sector = [0u8; 2352];
    file.read_exact(&mut sector)
        .map_err(|e| format!("Read error at LBA {lba}: {e}"))?;
    if descramble {
        let table = scramble_table();
        for i in 12..2352usize {
            sector[i] ^= table[i - 12];
        }
    }
    let mut out = [0u8; 2336];
    out.copy_from_slice(&sector[16..2352]);
    Ok(out)
}

fn parse_dir_records(buf: &[u8; BLOCK_SIZE]) -> Result<Vec<DiscEntry>, String> {
    let mut entries = Vec::new();
    let mut offset = 0usize;

    while offset < BLOCK_SIZE {
        let record_len = buf[offset] as usize;
        if record_len == 0 {
            break;
        }
        if offset + record_len > BLOCK_SIZE {
            break;
        }

        let r = &buf[offset..offset + record_len];
        offset += record_len;

        // Need at least 33 bytes for fixed-size fields
        if r.len() < 33 {
            continue;
        }

        // File beginning LBN: bytes 6-9 (BP 7-10, 4 bytes BE)
        let file_lba = u32::from_be_bytes([r[6], r[7], r[8], r[9]]);

        // File size: bytes 14-17 (BP 15-18, 4 bytes BE)
        let file_size = u32::from_be_bytes([r[14], r[15], r[16], r[17]]);

        // Creation date: bytes 18-23 (6 bytes: year-1900, month, day, hour, min, sec)
        let year = r[18] as u32 + 1900;
        let month = r[19];
        let day = r[20];
        let hour = r[21];
        let min = r[22];
        let sec = r[23];

        // File name size: byte 32 (BP 33)
        let name_size = r[32] as usize;
        let name_end = 33 + name_size;

        if name_end > r.len() {
            continue;
        }

        let name_bytes = &r[33..name_end];

        // Skip the "." entry (null byte name) and ".." entry (0x01 byte name)
        if name_size == 0
            || (name_size == 1 && (name_bytes[0] == 0x00 || name_bytes[0] == 0x01))
        {
            continue;
        }

        let name = std::str::from_utf8(name_bytes)
            .unwrap_or("???")
            .to_string();

        // Determine if directory from Attributes field (bit 15 = directory).
        // Layout after filename: if name_size is even, 1 padding byte; then 4 bytes Owner ID;
        // then 2 bytes Attributes (BP 38+n or 39+n depending on padding).
        let padding = if name_size % 2 == 0 { 1usize } else { 0usize };
        let attr_start = 33 + name_size + padding + 4; // skip Owner ID (4 bytes)

        let is_dir = if attr_start + 2 <= r.len() {
            let attr = u16::from_be_bytes([r[attr_start], r[attr_start + 1]]);
            attr & 0x8000 != 0
        } else {
            false
        };

        let modified = format!(
            "{}-{:02}-{:02} {:02}:{:02}:{:02}",
            year, month, day, hour, min, sec
        );

        entries.push(DiscEntry {
            deleted: false,
            name,
            is_dir,
            lba: file_lba,
            size: if is_dir { 0 } else { file_size },
            size_bytes: file_size,
            modified,
        });
    }

    Ok(entries)
}

/// Peek at the volume descriptor at LBA 16 and return true if it uses the CD-i
/// Green Book filesystem. Pass `descramble: true` for sectors stored scrambled
/// (ECMA-130 LFSR), e.g. the pregap of a CD-i Ready disc.
pub fn is_cdi_disc(
    bin_path: &std::path::Path,
    track_offset: u64,
    user_data_offset: u64,
    lba_offset: u64,
    descramble: bool,
) -> bool {
    let Ok(mut f) = File::open(bin_path) else { return false };
    let adjusted = if 16u64 >= lba_offset { 16 - lba_offset } else { 16 };
    if descramble {
        let pos = track_offset + adjusted * SECTOR_SIZE;
        if f.seek(SeekFrom::Start(pos)).is_err() { return false; }
        let mut sector = [0u8; 2352];
        if f.read_exact(&mut sector).is_err() { return false; }
        let table = scramble_table();
        for i in 12..2352usize { sector[i] ^= table[i - 12]; }
        // After descrambling, user data starts at user_data_offset. Green Book
        // volume descriptors usually put the type/identifier at bytes 8-13.
        let start = user_data_offset as usize;
        cdi_record_offset(&sector[start..]).is_some()
    } else {
        let pos = track_offset + adjusted * SECTOR_SIZE + user_data_offset;
        if f.seek(SeekFrom::Start(pos)).is_err() { return false; }
        let mut buf = [0u8; 14];
        if f.read_exact(&mut buf).is_err() { return false; }
        cdi_record_offset(&buf).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};

    #[test]
    fn opens_green_book_descriptor_with_lbn_prefix() {
        let path = std::env::temp_dir().join(format!(
            "disc-xplorer-cdi-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        let mut file = File::create(&path).unwrap();
        file.set_len(125 * SECTOR_SIZE).unwrap();

        let mut pvd = [0u8; BLOCK_SIZE];
        pvd[8] = 1;
        pvd[9..14].copy_from_slice(b"CD-I ");
        pvd[48..80].fill(b' ');
        pvd[48..57].copy_from_slice(b"TEST DISC");
        pvd[164..168].copy_from_slice(&17u32.to_be_bytes());
        file.seek(SeekFrom::Start(16 * SECTOR_SIZE + 24)).unwrap();
        file.write_all(&pvd).unwrap();

        let mut path_table = [0u8; BLOCK_SIZE];
        path_table[..4].copy_from_slice(&123u32.to_be_bytes());
        file.seek(SeekFrom::Start(17 * SECTOR_SIZE + 24)).unwrap();
        file.write_all(&path_table).unwrap();

        let mut root = [0u8; BLOCK_SIZE];
        root[0] = 42;
        root[6..10].copy_from_slice(&124u32.to_be_bytes());
        root[32] = 3;
        root[33..36].copy_from_slice(b"AUD");
        root[40..42].copy_from_slice(&0x8000u16.to_be_bytes());
        file.seek(SeekFrom::Start(123 * SECTOR_SIZE + 24)).unwrap();
        file.write_all(&root).unwrap();
        drop(file);

        let mut fs = CdiFs::new(File::open(&path).unwrap(), 0, 24, 0, false).unwrap();
        assert_eq!(fs.volume_id, "TEST DISC");
        assert_eq!(fs.root_lba, 123);
        assert!(is_cdi_disc(&path, 0, 24, 0, false));
        let entries = fs.list_directory("/").unwrap();
        assert_eq!(entries[0].name, "AUD");
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].lba, 124);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn recognizes_early_underscore_standard_id() {
        let mut descriptor = [0u8; 14];
        descriptor[8] = 1;
        descriptor[9..14].copy_from_slice(b"CD_I ");
        assert_eq!(cdi_record_offset(&descriptor), Some(8));
    }
}

/// Returns true when the pregap starting at `pregap_byte_offset` in the BIN
/// file contains ECMA-130-scrambled CD-i Mode 2 sectors (CD-i Ready format).
pub fn is_cdi_ready_pregap(bin_path: &std::path::Path, pregap_byte_offset: u64) -> bool {
    // Treat the pregap as a track starting at byte pregap_byte_offset, lba_offset=0.
    is_cdi_disc(bin_path, pregap_byte_offset, 24, 0, true)
}
