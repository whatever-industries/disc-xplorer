// KryoFlux raw stream decoding → plain sector image.
//
// A KryoFlux dump is a folder of per-track stream files (track00.0.raw =
// cylinder 0, head 0) holding raw flux-reversal timings sampled at ~24 MHz.
// This module parses the stream protocol, recovers the MFM bit clock with a
// simple PLL, locates IBM System/34 address/data marks (A1 sync, channel
// pattern 0x4489), verifies their CRCs, and assembles the good sectors into a
// plain 512-byte/sector disk image that the FAT layer can mount. Multiple
// captured revolutions act as free retries: the first CRC-clean copy of each
// sector wins.
//
// References:
//  - "KryoFlux Stream File Documentation" rev 1.1, Jean Louis-Guérin
//  - archivists-guide-to-kryoflux, "KryoFlux Stream Files" chapter
//  - DiskFormatID by Euan Cochrane (format-identification approach)

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

// ── Stream file parsing ─────────────────────────────────────────────────────

struct RawStream {
    // Flux-reversal durations in sample-clock (sck) ticks.
    flux: Vec<u32>,
}

fn parse_stream(data: &[u8]) -> RawStream {
    let mut flux: Vec<u32> = Vec::with_capacity(data.len());
    let mut overflow = 0u32;
    let mut i = 0usize;
    while i < data.len() {
        let h = data[i];
        match h {
            // Flux2: two-byte value, high bits in the header.
            0x00..=0x07 => {
                if i + 1 >= data.len() { break; }
                flux.push(overflow + ((h as u32) << 8) + data[i + 1] as u32);
                overflow = 0;
                i += 2;
            }
            // NOP1..NOP3: padding, skip.
            0x08 => i += 1,
            0x09 => i += 2,
            0x0A => i += 3,
            // Ovl16: next flux value is 0x10000 higher (sample counter overflow).
            0x0B => {
                overflow += 0x10000;
                i += 1;
            }
            // Flux3: three-byte block, 16-bit value.
            0x0C => {
                if i + 2 >= data.len() { break; }
                flux.push(overflow + ((data[i + 1] as u32) << 8) + data[i + 2] as u32);
                overflow = 0;
                i += 3;
            }
            // OOB block: 0x0D, type, u16 size, payload. Sent asynchronously and
            // not part of the stream buffer. We only need EOF; index and
            // hardware-info blocks are irrelevant to sector-level decoding.
            0x0D => {
                if i + 4 > data.len() { break; }
                let typ = data[i + 1];
                if typ == 0x0D { break; } // EOF: size field is meaningless
                let size = u16::from_le_bytes([data[i + 2], data[i + 3]]) as usize;
                i += 4 + size;
            }
            // Flux1: single-byte value.
            0x0E..=0xFF => {
                flux.push(overflow + h as u32);
                overflow = 0;
                i += 1;
            }
        }
    }
    RawStream { flux }
}

// ── MFM decoding ────────────────────────────────────────────────────────────

// Estimate the MFM channel-bit cell (in sck ticks) from the flux histogram.
// On an MFM disk flux intervals cluster at 2, 3 and 4 channel cells; the
// first (shortest) peak is the 2-cell class. DD media ≈ 96 ticks, HD ≈ 48.
fn estimate_cell(flux: &[u32]) -> Option<f64> {
    let mut hist = [0u32; 512];
    for &f in flux {
        if (f as usize) < 512 {
            hist[f as usize] += 1;
        }
    }
    let best = *hist.iter().max()?;
    if best < 32 {
        return None; // not enough signal for a histogram
    }
    let thresh = best / 4;
    // First local maximum above the threshold, scanning from short intervals.
    let mut i = 8;
    while i < 511 {
        if hist[i] >= thresh && hist[i] >= hist[i - 1] && hist[i] >= hist[i + 1] {
            // Refine: centroid over the ±4 neighborhood.
            let (mut num, mut den) = (0f64, 0f64);
            for j in i.saturating_sub(4)..=(i + 4).min(511) {
                num += (j as f64) * hist[j] as f64;
                den += hist[j] as f64;
            }
            return Some(num / den / 2.0);
        }
        i += 1;
    }
    None
}

// Convert flux durations to a channel-bit stream with a proportional PLL.
fn flux_to_bits(flux: &[u32], cell0: f64) -> Vec<u8> {
    let mut bits: Vec<u8> = Vec::with_capacity(flux.len() * 4);
    let mut cell = cell0;
    for &f in flux {
        let n = ((f as f64 / cell).round() as i64).clamp(2, 8);
        for _ in 0..n - 1 {
            bits.push(0);
        }
        bits.push(1);
        // Nudge the clock toward the observed phase error.
        let err = f as f64 - n as f64 * cell;
        cell = (cell + err / n as f64 * 0.05).clamp(cell0 * 0.85, cell0 * 1.15);
    }
    bits
}

// CRC-16/CCITT as used by the floppy controller (poly 0x1021, init 0xFFFF).
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

pub struct DecodedSector {
    pub cyl: u8,
    pub head: u8,
    pub sec: u8,
    pub data: Vec<u8>,
    pub crc_ok: bool,
}

// Decode one data byte from MFM channel bits (clock, data pairs).
fn read_byte(bits: &[u8], pos: &mut usize) -> Option<u8> {
    if *pos + 16 > bits.len() {
        return None;
    }
    let mut b = 0u8;
    for k in 0..8 {
        b = (b << 1) | bits[*pos + 2 * k + 1];
    }
    *pos += 16;
    Some(b)
}

// Scan a channel-bit stream for IBM address/data marks and decode sectors.
fn scan_bits(bits: &[u8]) -> Vec<DecodedSector> {
    // Three A1 sync bytes with the missing-clock violation: 0x4489 ×3.
    const SYNC3: u64 = 0x4489_4489_4489;
    const SYNC3_MASK: u64 = 0xFFFF_FFFF_FFFF;
    let mut out = Vec::new();
    let mut sr: u64 = 0;
    let mut i = 0usize;
    // The most recent CRC-valid ID mark, waiting for its data mark.
    let mut pending_id: Option<(u8, u8, u8, usize, usize)> = None; // cyl, head, sec, size, bitpos
    while i < bits.len() {
        sr = (sr << 1) | bits[i] as u64;
        i += 1;
        if i < 48 || (sr & SYNC3_MASK) != SYNC3 {
            continue;
        }
        let mut pos = i;
        let Some(mark) = read_byte(bits, &mut pos) else { break };
        match mark {
            // ID address mark: cyl, head, sector, size code, CRC.
            0xFE => {
                let mut hdr = [0u8; 6];
                let mut ok = true;
                for slot in hdr.iter_mut() {
                    match read_byte(bits, &mut pos) {
                        Some(b) => *slot = b,
                        None => { ok = false; break; }
                    }
                }
                if ok {
                    let crc = crc16(&[0xA1, 0xA1, 0xA1, 0xFE, hdr[0], hdr[1], hdr[2], hdr[3]]);
                    if crc == u16::from_be_bytes([hdr[4], hdr[5]]) {
                        let size = 128usize << (hdr[3] & 3);
                        pending_id = Some((hdr[0], hdr[1], hdr[2], size, pos));
                    }
                }
                i = pos;
            }
            // Data mark (0xF8 = "deleted data" control mark, still real data).
            0xFB | 0xF8 => {
                if let Some((c, h, s, size, id_pos)) = pending_id.take() {
                    // The data mark belongs to the ID only if it follows within
                    // the inter-record gap (< ~60 bytes of channel bits).
                    if pos - id_pos < 16 * 60 {
                        let mut data = vec![0u8; size];
                        let mut ok = true;
                        for b in data.iter_mut() {
                            match read_byte(bits, &mut pos) {
                                Some(v) => *b = v,
                                None => { ok = false; break; }
                            }
                        }
                        let mut crc_ok = false;
                        if ok {
                            if let (Some(ch), Some(cl)) = (read_byte(bits, &mut pos), read_byte(bits, &mut pos)) {
                                let mut buf = Vec::with_capacity(size + 4);
                                buf.extend_from_slice(&[0xA1, 0xA1, 0xA1, mark]);
                                buf.extend_from_slice(&data);
                                crc_ok = crc16(&buf) == ((ch as u16) << 8 | cl as u16);
                            }
                        }
                        out.push(DecodedSector { cyl: c, head: h, sec: s, data, crc_ok });
                    }
                }
                i = pos;
            }
            _ => {}
        }
        sr = 0;
    }
    out
}

pub fn decode_track(stream_bytes: &[u8]) -> Vec<DecodedSector> {
    let stream = parse_stream(stream_bytes);
    let Some(cell) = estimate_cell(&stream.flux) else { return vec![] };
    scan_bits(&flux_to_bits(&stream.flux, cell))
}

// ── Stream set discovery & disk assembly ───────────────────────────────────

// Given any stream file, find (directory, prefix) of its dump set. DTC names
// files "<prefix>NN.S.raw" — two-digit cylinder, one-digit side. The prefix is
// free-form: "track00.0.raw", "My Disk(1of1)26.1.raw", etc.
fn stream_set(path: &Path) -> Option<(PathBuf, String)> {
    let name = path.file_name()?.to_str()?.to_lowercase();
    let stem = name.strip_suffix(".raw")?;
    let b = stem.as_bytes();
    let n = b.len();
    if n >= 4
        && b[n - 1].is_ascii_digit()
        && b[n - 2] == b'.'
        && b[n - 3].is_ascii_digit()
        && b[n - 4].is_ascii_digit()
    {
        Some((path.parent()?.to_path_buf(), stem[..n - 4].to_string()))
    } else {
        None
    }
}

// Cheap check used by the format dispatchers: is this a KryoFlux stream file?
pub fn is_kryoflux_stream(path: &Path) -> bool {
    stream_set(path).is_some()
}

pub struct FloppyImage {
    pub data: Arc<[u8]>,
    pub good_sectors: usize,
    pub bad_sectors: usize,
}

// Decode a whole dump set into a flat 512-byte/sector image.
fn assemble(dir: &Path, prefix: &str) -> Result<FloppyImage, String> {
    // (cyl, head, sec) → (data, crc_ok); the first CRC-clean copy wins.
    let mut sectors: HashMap<(u8, u8, u8), (Vec<u8>, bool)> = HashMap::new();
    let mut max_cyl = 0u8;
    let mut max_head = 0u8;
    let mut max_sec = 0u8;
    let mut any = false;

    let entries = fs::read_dir(dir).map_err(|e| format!("Cannot read dump folder: {e}"))?;
    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().to_lowercase();
        if !fname.starts_with(prefix) || !fname.ends_with(".raw") {
            continue;
        }
        let mid = &fname[prefix.len()..fname.len() - 4];
        // Expect "##.#"
        let parts: Vec<&str> = mid.split('.').collect();
        if parts.len() != 2 {
            continue;
        }
        let (Ok(_cyl), Ok(_head)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) else {
            continue;
        };
        let mut bytes = Vec::new();
        if read_file_bytes(&entry.path(), &mut bytes).is_err() {
            continue;
        }
        for s in decode_track(&bytes) {
            if s.data.len() != 512 {
                continue; // FAT floppies use 512-byte sectors
            }
            if s.sec == 0 || s.sec > 36 || s.cyl > 84 {
                continue; // implausible ID — noise
            }
            any = true;
            max_cyl = max_cyl.max(s.cyl);
            max_head = max_head.max(s.head);
            max_sec = max_sec.max(s.sec);
            let key = (s.cyl, s.head, s.sec);
            match sectors.get(&key) {
                Some((_, true)) => {}                        // already have a good copy
                Some((_, false)) if !s.crc_ok => {}          // keep the earlier bad copy
                _ => { sectors.insert(key, (s.data, s.crc_ok)); }
            }
        }
    }
    if !any {
        return Err("No decodable MFM sectors found in the stream set".to_string());
    }

    // Geometry: prefer the BPB in the boot sector; fall back to what we saw.
    let mut heads = max_head as usize + 1;
    let mut spt = max_sec as usize;
    let mut total_sectors = (max_cyl as usize + 1) * heads * spt;
    if let Some((boot, true)) = sectors.get(&(0, 0, 1)) {
        let bps = u16::from_le_bytes([boot[11], boot[12]]) as usize;
        let bpb_spt = u16::from_le_bytes([boot[24], boot[25]]) as usize;
        let bpb_heads = u16::from_le_bytes([boot[26], boot[27]]) as usize;
        let bpb_total = u16::from_le_bytes([boot[19], boot[20]]) as usize;
        if bps == 512 && (1..=36).contains(&bpb_spt) && (1..=2).contains(&bpb_heads) && bpb_total > 0 {
            spt = bpb_spt;
            heads = bpb_heads;
            total_sectors = bpb_total;
        }
    }

    let mut data = vec![0u8; total_sectors * 512];
    let (mut good, mut bad) = (0usize, 0usize);
    for ((cyl, head, sec), (payload, crc_ok)) in &sectors {
        let lba = (*cyl as usize * heads + *head as usize) * spt + (*sec as usize - 1);
        let off = lba * 512;
        if off + 512 <= data.len() {
            data[off..off + 512].copy_from_slice(payload);
            if *crc_ok { good += 1 } else { bad += 1 }
        }
    }
    Ok(FloppyImage { data: data.into(), good_sectors: good, bad_sectors: bad })
}

fn read_file_bytes(path: &Path, out: &mut Vec<u8>) -> std::io::Result<()> {
    fs::File::open(path)?.read_to_end(out).map(|_| ())
}

// ── Cached loader ───────────────────────────────────────────────────────────

// Keyed by (dir, prefix); fingerprint = sum of file sizes so a re-dump refreshes.
type CacheMap = HashMap<(PathBuf, String), (u64, Arc<[u8]>)>;
static IMAGE_CACHE: OnceLock<Mutex<CacheMap>> = OnceLock::new();

fn set_fingerprint(dir: &Path, prefix: &str) -> u64 {
    let mut sum = 0u64;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.starts_with(prefix) && n.ends_with(".raw") {
                sum = sum.wrapping_add(e.metadata().map(|m| m.len()).unwrap_or(0)).wrapping_add(1);
            }
        }
    }
    sum
}

// Decode (or fetch from cache) the disk image for the dump set containing `path`.
pub fn floppy_image(path: &Path) -> Result<Arc<[u8]>, String> {
    let (dir, prefix) = stream_set(path).ok_or("Not a KryoFlux stream file")?;
    let fp = set_fingerprint(&dir, &prefix);
    let cache = IMAGE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some((cached_fp, img)) = guard.get(&(dir.clone(), prefix.clone())) {
            if *cached_fp == fp {
                return Ok(img.clone());
            }
        }
    }
    let img = assemble(&dir, &prefix)?;
    if let Ok(mut guard) = cache.lock() {
        guard.insert((dir, prefix), (fp, img.data.clone()));
    }
    Ok(img.data)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fat_filesystem::FatFs;
    use std::io::Cursor;

    #[test]
    fn parses_stream_block_types() {
        // Flux1 (0x60), Flux2 (0x03,0x20 → 0x320), NOP1, Flux3 (0x1234),
        // Ovl16 + Flux1 (0x10060), OOB StreamInfo (skipped), Flux1, OOB EOF.
        let data = [
            0x60,
            0x03, 0x20,
            0x08,
            0x0C, 0x12, 0x34,
            0x0B, 0x60,
            0x0D, 0x01, 0x08, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
            0x70,
            0x0D, 0x0D, 0x0D, 0x0D,
            0xFF, // after EOF: must be ignored
        ];
        let s = parse_stream(&data);
        assert_eq!(s.flux, vec![0x60, 0x320, 0x1234, 0x10060, 0x70]);
    }

    // ── MFM encoding helpers (test-only, the inverse of the decoder) ──────

    fn mfm_byte(out: &mut Vec<u8>, byte: u8, last: &mut u8) {
        for k in (0..8).rev() {
            let d = (byte >> k) & 1;
            let clock = u8::from(*last == 0 && d == 0);
            out.push(clock);
            out.push(d);
            *last = d;
        }
    }

    fn mfm_sync_a1(out: &mut Vec<u8>, last: &mut u8) {
        // A1 with the missing-clock violation: channel pattern 0x4489.
        for k in (0..16).rev() {
            out.push(((0x4489u16 >> k) & 1) as u8);
        }
        *last = 1;
    }

    fn encode_track_stream(image: &[u8], cyl: u8, head: u8, spt: usize) -> Vec<u8> {
        let mut bits: Vec<u8> = Vec::new();
        let mut last = 0u8;
        for _ in 0..32 { mfm_byte(&mut bits, 0x4E, &mut last); }
        for sec in 1..=spt {
            let payload = &image[(sec - 1) * 512..sec * 512];
            for _ in 0..12 { mfm_byte(&mut bits, 0x00, &mut last); }
            for _ in 0..3 { mfm_sync_a1(&mut bits, &mut last); }
            let hdr = [cyl, head, sec as u8, 2u8]; // size code 2 = 512
            let crc = crc16(&[0xA1, 0xA1, 0xA1, 0xFE, hdr[0], hdr[1], hdr[2], hdr[3]]);
            mfm_byte(&mut bits, 0xFE, &mut last);
            for b in hdr { mfm_byte(&mut bits, b, &mut last); }
            for b in crc.to_be_bytes() { mfm_byte(&mut bits, b, &mut last); }
            for _ in 0..22 { mfm_byte(&mut bits, 0x4E, &mut last); }
            for _ in 0..12 { mfm_byte(&mut bits, 0x00, &mut last); }
            for _ in 0..3 { mfm_sync_a1(&mut bits, &mut last); }
            mfm_byte(&mut bits, 0xFB, &mut last);
            let mut crc_buf = vec![0xA1, 0xA1, 0xA1, 0xFB];
            crc_buf.extend_from_slice(payload);
            for &b in payload { mfm_byte(&mut bits, b, &mut last); }
            for b in crc16(&crc_buf).to_be_bytes() { mfm_byte(&mut bits, b, &mut last); }
            for _ in 0..24 { mfm_byte(&mut bits, 0x4E, &mut last); }
        }
        for _ in 0..64 { mfm_byte(&mut bits, 0x4E, &mut last); }

        // Channel bits → flux durations (DD: 48 sck ticks per channel cell).
        const CELL: u32 = 48;
        let mut flux: Vec<u32> = Vec::new();
        let mut run = 0u32;
        for &b in &bits {
            run += 1;
            if b == 1 {
                flux.push(run * CELL);
                run = 0;
            }
        }
        // Flux values → stream bytes (Flux1/Flux2 as size dictates).
        let mut stream: Vec<u8> = Vec::new();
        for f in flux {
            if (0x0E..=0xFF).contains(&f) {
                stream.push(f as u8);
            } else {
                assert!(f <= 0x7FF, "test flux out of Flux2 range");
                stream.push((f >> 8) as u8);
                stream.push((f & 0xFF) as u8);
            }
        }
        stream.extend_from_slice(&[0x0D, 0x0D, 0x0D, 0x0D]); // EOF
        stream
    }

    // Minimal 9-sector FAT12 floppy: boot + FAT + root + 6 data sectors.
    // HELLO.TXT lives in cluster 2; GONE.TXT was deleted (0xE5, freed chain)
    // and its data sits in cluster 3.
    fn build_fat12_image() -> Vec<u8> {
        let mut img = vec![0u8; 9 * 512];
        // Boot sector / BPB.
        img[0] = 0xEB; img[1] = 0x3C; img[2] = 0x90;
        img[11..13].copy_from_slice(&512u16.to_le_bytes());
        img[13] = 1; // sectors per cluster
        img[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved
        img[16] = 1; // FATs
        img[17..19].copy_from_slice(&16u16.to_le_bytes()); // root entries
        img[19..21].copy_from_slice(&9u16.to_le_bytes()); // total sectors
        img[21] = 0xF0; // media descriptor
        img[22..24].copy_from_slice(&1u16.to_le_bytes()); // FAT size
        img[24..26].copy_from_slice(&9u16.to_le_bytes()); // sectors per track
        img[26..28].copy_from_slice(&1u16.to_le_bytes()); // heads
        img[510] = 0x55; img[511] = 0xAA;
        // FAT12 (sector 1): entries 0/1 reserved; cluster 2 = EOC; 3 = free.
        let fat = 512;
        img[fat] = 0xF0; img[fat + 1] = 0xFF; img[fat + 2] = 0xFF;
        img[fat + 3] = 0xFF; img[fat + 4] = 0x0F;
        // Root dir (sector 2).
        let root = 2 * 512;
        img[root..root + 11].copy_from_slice(b"HELLO   TXT");
        img[root + 11] = 0x20;
        img[root + 26..root + 28].copy_from_slice(&2u16.to_le_bytes());
        img[root + 28..root + 32].copy_from_slice(&17u32.to_le_bytes());
        let e2 = root + 32;
        img[e2..e2 + 11].copy_from_slice(b"GONE    TXT");
        img[e2] = 0xE5; // deleted marker over the 'G'
        img[e2 + 11] = 0x20;
        img[e2 + 26..e2 + 28].copy_from_slice(&3u16.to_le_bytes());
        img[e2 + 28..e2 + 32].copy_from_slice(&4u32.to_le_bytes());
        // Data: cluster 2 = sector 3, cluster 3 = sector 4.
        img[3 * 512..3 * 512 + 17].copy_from_slice(b"hello from floppy");
        img[4 * 512..4 * 512 + 4].copy_from_slice(b"gone");
        img
    }

    #[test]
    fn roundtrip_fat12_floppy_with_undelete() {
        let img = build_fat12_image();
        let stream = encode_track_stream(&img, 0, 0, 9);

        let dir = std::env::temp_dir().join("kryoflux_roundtrip_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let track = dir.join("track00.0.raw");
        fs::write(&track, &stream).unwrap();

        assert!(is_kryoflux_stream(&track));
        let data = floppy_image(&track).unwrap();
        assert_eq!(data.len(), 9 * 512);
        assert_eq!(&data[..512], &img[..512], "boot sector must round-trip");

        let mut fatfs = FatFs::new(Cursor::new(data)).unwrap();
        assert_eq!(fatfs.label, "FAT12");
        let root = fatfs.list_directory("/").unwrap();
        assert_eq!(root.len(), 2);
        let hello = root.iter().find(|e| e.name == "HELLO.TXT").unwrap();
        assert!(!hello.deleted);
        assert_eq!(hello.size_bytes, 17);
        // Deleted entry: first name byte is lost to the 0xE5 marker.
        let gone = root.iter().find(|e| e.name == "_ONE.TXT").unwrap();
        assert!(gone.deleted);

        let d1 = std::env::temp_dir().join("kryoflux_test_hello.txt");
        fatfs.extract_file("/HELLO.TXT", d1.to_str().unwrap()).unwrap();
        assert_eq!(fs::read(&d1).unwrap(), b"hello from floppy");
        let d2 = std::env::temp_dir().join("kryoflux_test_gone.txt");
        fatfs.extract_file("/_ONE.TXT", d2.to_str().unwrap()).unwrap();
        assert_eq!(fs::read(&d2).unwrap(), b"gone");

        let _ = fs::remove_file(&d1);
        let _ = fs::remove_file(&d2);
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod real_dump_tests {
    use super::*;

    // Compare our flux decode against a DTC-produced .img of the same dump.
    // Run: DX_KF_RAW=<any .raw of the set> DX_KF_IMG=<reference .img> \
    //      cargo test --release real_dump -- --ignored --nocapture
    #[test]
    #[ignore]
    fn matches_dtc_reference_image() {
        let raw = std::env::var("DX_KF_RAW").expect("set DX_KF_RAW");
        let img = std::env::var("DX_KF_IMG").expect("set DX_KF_IMG");
        let ours = floppy_image(Path::new(&raw)).expect("decode failed");
        let reference = fs::read(&img).expect("read reference");
        println!("ours: {} bytes, reference: {} bytes", ours.len(), reference.len());
        let n = ours.len().min(reference.len());
        let mut diff_sectors = 0usize;
        for s in 0..n / 512 {
            if ours[s * 512..(s + 1) * 512] != reference[s * 512..(s + 1) * 512] {
                if diff_sectors < 10 {
                    println!("sector {} differs", s);
                }
                diff_sectors += 1;
            }
        }
        println!("differing sectors: {diff_sectors} of {}", n / 512);
        assert_eq!(ours.len(), reference.len(), "image size mismatch");
        assert_eq!(diff_sectors, 0, "content mismatch vs DTC reference");
    }
}

#[cfg(test)]
mod real_dump_listing {
    use super::*;
    use crate::fat_filesystem::FatFs;
    use std::io::Cursor;

    // Mount a real dump and print the root listing.
    // Run: DX_KF_RAW=<any .raw> cargo test --release real_dump_listing -- --ignored --nocapture
    #[test]
    #[ignore]
    fn lists_real_dump() {
        let raw = std::env::var("DX_KF_RAW").expect("set DX_KF_RAW");
        let img = floppy_image(Path::new(&raw)).expect("decode failed");
        println!("image: {} bytes ({} sectors)", img.len(), img.len() / 512);
        let mut fatfs = FatFs::new(Cursor::new(img)).expect("not FAT");
        println!("filesystem: {}", fatfs.label);
        let mut stack = vec![String::from("/")];
        let mut files = 0;
        while let Some(dir) = stack.pop() {
            for e in fatfs.list_directory(&dir).expect("list") {
                let flag = if e.deleted { " [deleted]" } else { "" };
                println!("{:>9}  {}  {}{}{}", e.size_bytes, e.modified, dir.trim_end_matches('/'), format!("/{}", e.name), flag);
                if e.is_dir && !e.deleted {
                    stack.push(format!("{}/{}", dir.trim_end_matches('/'), e.name));
                }
                files += 1;
            }
        }
        println!("total entries: {files}");
        // Optional extraction smoke test: DX_KF_EXTRACT=/PATH/IN/IMAGE
        if let Ok(p) = std::env::var("DX_KF_EXTRACT") {
            let dest = std::env::temp_dir().join("kf_extract_test.bin");
            fatfs.extract_file(&p, dest.to_str().unwrap()).expect("extract");
            let data = fs::read(&dest).unwrap();
            let printable = data.iter().filter(|&&b| (0x20..0x7F).contains(&b) || b == b'\r' || b == b'\n' || b == b'\t').count();
            println!("extracted {}: {} bytes, {}% printable", p, data.len(), printable * 100 / data.len().max(1));
            println!("first line: {}", String::from_utf8_lossy(&data[..data.len().min(80)]).lines().next().unwrap_or(""));
            let _ = fs::remove_file(&dest);
        }
    }
}
