// PS3 disc image encryption / decryption.
//
// A PS3 Blu-ray game image is split into an odd number of "regions". Even
// regions (0,2,4…) are unencrypted; odd regions (1,3,5…) are AES-128-CBC
// encrypted with the per-disc key (D1 / data1). Each 2048-byte sector is its
// own CBC unit with IV = 12 zero bytes followed by the absolute sector LBA
// (big-endian, last 4 bytes). Every sector in an encrypted region is processed,
// including all-zero plaintext sectors — this matches Redump's encrypted images
// (the older 3k3y tools skipped zero sectors, which yields a different hash).
//
// Region table (first sector):
//   offset 0x00 : u32 BE  number of plain regions N (total regions = 2N-1)
//   offset 0x0C : u32 BE[] region boundary sectors (one per region)
//
// 3k3y ISOs embed a watermark at 0xF70 ("Encrypted 3K BLD" / "Dncrypted 3K
// BLD") and the D1 key at 0xF80. Redump ISOs carry the key in an external
// .dkey/.key file.
//
// Sources (both MIT): ps3dec by Yacine S. (github.com/Redrrx/ps3dec) for the
// per-sector AES-CBC core and IV scheme; ps3netsrv (aldostools / NvrBst) for
// the region-table parsing and 3k3y watermark detection.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write, BufReader, BufWriter};
use std::path::{Path, PathBuf};

use aes::Aes128;
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::NoPadding};

type AesDec = cbc::Decryptor<Aes128>;
type AesEnc = cbc::Encryptor<Aes128>;

const SECTOR: usize = 2048;
const WATERMARK_OFF: usize = 0xF70;
const WATERMARK_ENC: &[u8; 16] = b"Encrypted 3K BLD";
const WATERMARK_DEC: &[u8; 16] = b"Dncrypted 3K BLD";

#[derive(Clone, Copy)]
pub struct Region {
    pub first: u64, // first sector (inclusive)
    pub last: u64,  // last sector (inclusive)
    pub encrypted: bool,
}

pub struct Ps3Detection {
    pub encrypted: bool,
    pub regions: Vec<Region>,
    pub total_sectors: u64,
}

/// Parse the region table from the first sector. Returns None if the header
/// does not look like a PS3 region table.
fn parse_regions(header: &[u8], total_sectors: u64) -> Option<Vec<Region>> {
    if header.len() < 16 {
        return None;
    }
    let plain_count = u32::from_be_bytes(header[0..4].try_into().ok()?) as usize;
    if plain_count == 0 || plain_count >= 32 {
        return None;
    }
    let total = plain_count * 2 - 1;
    if header.len() < 12 + total * 4 {
        return None;
    }
    let mut regions = Vec::with_capacity(total);
    let mut prev_last: u64 = 0;
    for i in 0..total {
        let encrypted = i % 2 == 1;
        let off = 12 + i * 4;
        let boundary = u32::from_be_bytes(header[off..off + 4].try_into().ok()?) as u64;
        // ps3netsrv: encrypted regions' boundary is exclusive (subtract 1).
        let last = boundary.saturating_sub(if encrypted { 1 } else { 0 });
        let first = if i == 0 { 0 } else { prev_last + 1 };
        if last < first {
            return None;
        }
        regions.push(Region { first, last, encrypted });
        prev_last = last;
    }
    // Sanity: the final boundary should be within the image.
    if regions.last().map(|r| r.last) > Some(total_sectors) {
        return None;
    }
    Some(regions)
}

#[inline]
fn sector_iv(lba: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[12..16].copy_from_slice(&(lba as u32).to_be_bytes());
    iv
}

#[inline]
fn is_encrypted_sector(regions: &[Region], lba: u64) -> bool {
    regions.iter().any(|r| r.encrypted && lba >= r.first && lba <= r.last)
}

/// Read the first two sectors (region table + watermark area).
fn read_header(path: &Path) -> io::Result<(Vec<u8>, u64)> {
    let mut f = File::open(path)?;
    let total = f.metadata()?.len();
    let mut buf = vec![0u8; SECTOR * 2];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok((buf, total))
}

/// Detect whether `path` is a PS3 ISO and, if so, its encryption state.
/// Returns None when the file is not a PS3 disc image.
pub fn detect(path: &Path) -> Option<Ps3Detection> {
    let (header, total) = read_header(path).ok()?;
    if total < (SECTOR * 2) as u64 || total % SECTOR as u64 != 0 {
        return None;
    }
    let total_sectors = total / SECTOR as u64;
    let regions = parse_regions(&header, total_sectors)?;

    // 3k3y watermark gives a direct answer when present.
    if header.len() >= WATERMARK_OFF + 16 {
        let mark = &header[WATERMARK_OFF..WATERMARK_OFF + 16];
        if mark == WATERMARK_ENC {
            return Some(Ps3Detection { encrypted: true, regions, total_sectors });
        }
        if mark == WATERMARK_DEC {
            return Some(Ps3Detection { encrypted: false, regions, total_sectors });
        }
    }

    // Redump: probe a known plaintext anchor that lives in an encrypted region.
    let encrypted = match probe_decrypted(path, &regions) {
        Some(decrypted) => !decrypted,
        // Anchor unresolved: fall back to "encrypted" (the common redump case);
        // detection is confirmed later when a key validates the conversion.
        None => true,
    };
    Some(Ps3Detection { encrypted, regions, total_sectors })
}

/// Returns Some(true) if the image is decrypted, Some(false) if encrypted,
/// None if we could not find a usable anchor. Looks for the EBOOT.BIN SELF
/// header ("SCE\0") which only appears in plaintext on a decrypted disc.
fn probe_decrypted(path: &Path, regions: &[Region]) -> Option<bool> {
    let lba = eboot_lba(path)?;
    if !is_encrypted_sector(regions, lba) {
        return None; // anchor isn't in an encrypted region — inconclusive
    }
    let mut f = File::open(path).ok()?;
    f.seek(SeekFrom::Start(lba * SECTOR as u64)).ok()?;
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).ok()?;
    Some(&buf == b"SCE\0")
}

/// Resolve the LBA of /PS3_GAME/USRDIR/EBOOT.BIN via the ISO9660 directory,
/// which is plaintext (lives in the unencrypted region).
fn eboot_lba(path: &Path) -> Option<u64> {
    let file = File::open(path).ok()?;
    let iso = iso9660::ISO9660::new(file).ok()?;
    let usrdir = match iso.open("/PS3_GAME/USRDIR").ok()?? {
        iso9660::DirectoryEntry::Directory(d) => d,
        _ => return None,
    };
    for entry in usrdir.contents() {
        let entry = entry.ok()?;
        let id = entry.identifier();
        if id.eq_ignore_ascii_case("EBOOT.BIN") || id.eq_ignore_ascii_case("EBOOT.BIN;1") {
            return Some(entry.header().extent_loc as u64);
        }
    }
    None
}

/// Find a sibling key file (same stem, .dkey or .key) next to the ISO.
pub fn find_key_file(iso_path: &Path) -> Option<PathBuf> {
    let stem = iso_path.file_stem()?;
    let dir = iso_path.parent()?;
    for ext in ["dkey", "key"] {
        let mut p = dir.join(stem);
        p.set_extension(ext);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Load a 16-byte AES key from a .key (16 raw bytes) or .dkey (32 hex chars).
pub fn load_key(key_path: &Path) -> Result<[u8; 16], String> {
    let data = std::fs::read(key_path).map_err(|e| format!("Cannot read key: {e}"))?;
    if data.len() == 16 {
        let mut k = [0u8; 16];
        k.copy_from_slice(&data);
        return Ok(k);
    }
    // Otherwise treat as ASCII hex (ignore whitespace / control chars).
    let hex: String = data
        .iter()
        .map(|&b| b as char)
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if hex.len() != 32 {
        return Err(format!(
            "Key must be 16 raw bytes or 32 hex chars (got {} usable hex chars)",
            hex.len()
        ));
    }
    let mut k = [0u8; 16];
    for i in 0..16 {
        k[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "Invalid hex in key file".to_string())?;
    }
    Ok(k)
}

/// Available free space (bytes) on the volume containing `path` or its parent.
pub fn available_space(path: &Path) -> Option<u64> {
    let probe = if path.exists() { path } else { path.parent()? };
    fs2::available_space(probe).ok()
}

/// Convert an image in place to a new file: decrypt if `encrypt` is false,
/// encrypt if true. `progress` is invoked with (sectors_done, total_sectors)
/// and returns `false` to request cancellation; on cancel the partial output
/// file is removed and `Err("__cancelled__")` is returned.
pub fn convert<F: FnMut(u64, u64) -> bool>(
    in_path: &Path,
    out_path: &Path,
    key: &[u8; 16],
    encrypt: bool,
    mut progress: F,
) -> Result<(), String> {
    let in_file = File::open(in_path).map_err(|e| format!("Cannot open input: {e}"))?;
    let total = in_file.metadata().map_err(|e| e.to_string())?.len();
    if total % SECTOR as u64 != 0 {
        return Err("Image size is not a multiple of 2048".to_string());
    }
    let total_sectors = total / SECTOR as u64;

    let (header, _) = read_header(in_path).map_err(|e| e.to_string())?;
    let regions = parse_regions(&header, total_sectors)
        .ok_or_else(|| "Not a recognizable PS3 region table".to_string())?;

    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, in_file);
    let out_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(out_path)
        .map_err(|e| format!("Cannot create output: {e}"))?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);

    const CHUNK_SECTORS: usize = 2048; // 4 MiB
    let mut buf = vec![0u8; SECTOR * CHUNK_SECTORS];
    let mut lba: u64 = 0;
    while lba < total_sectors {
        let n = ((total_sectors - lba) as usize).min(CHUNK_SECTORS);
        let bytes = n * SECTOR;
        reader.read_exact(&mut buf[..bytes]).map_err(|e| format!("Read error: {e}"))?;

        for (i, sector) in buf[..bytes].chunks_mut(SECTOR).enumerate() {
            let s = lba + i as u64;
            if !is_encrypted_sector(&regions, s) {
                continue;
            }
            let iv = sector_iv(s);
            if encrypt {
                AesEnc::new(key.into(), &iv.into())
                    .encrypt_padded_mut::<NoPadding>(sector, SECTOR)
                    .map_err(|e| format!("Encrypt error at sector {s}: {e}"))?;
            } else {
                AesDec::new(key.into(), &iv.into())
                    .decrypt_padded_mut::<NoPadding>(sector)
                    .map_err(|e| format!("Decrypt error at sector {s}: {e}"))?;
            }
        }

        writer.write_all(&buf[..bytes]).map_err(|e| format!("Write error: {e}"))?;
        lba += n as u64;
        if !progress(lba, total_sectors) {
            drop(writer);
            let _ = std::fs::remove_file(out_path);
            return Err("__cancelled__".to_string());
        }
    }
    writer.flush().map_err(|e| format!("Flush error: {e}"))?;
    Ok(())
}
