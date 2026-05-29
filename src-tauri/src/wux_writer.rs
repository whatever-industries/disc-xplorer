// WUX (Wii U compressed disc image) writer — the inverse of `wux_reader`.
//
// Takes a raw Wii U image (.wud or raw .iso) and produces a .wux that
// deduplicates `sector_size`-byte blocks: identical blocks (e.g. padding) are
// stored once and referenced by multiple index-table entries.
//
// File layout (all multi-byte integers little-endian) — see wux_reader.rs:
//   Header (32 bytes):
//     [0x00] u32  magic0 = 0x30585557 ("WUX0")
//     [0x04] u32  magic1 = 0x1099D02E
//     [0x08] u32  sectorSize — block size in bytes (power-of-2, typically 32768)
//     [0x0C] u32  (reserved)
//     [0x10] u64  uncompressedSize — total disc size in bytes
//     [0x18] u32  flags
//     [0x1C] u32  (reserved)
//   Index table @ 0x20: numSectors = ceil(size/sectorSize) entries of u32 LE.
//   Data @ ALIGN(0x20 + numSectors*4, sectorSize): unique blocks, each
//     sectorSize bytes, in physical-index order.
//
// Block identity is keyed on a BLAKE3 hash of the full (zero-padded) block.
// Collision probability is cryptographically negligible, so blocks are not
// re-compared; callers wanting a guarantee can run the opt-in round-trip verify.
//
// Spec source: WudCompress (CEMU project, MIT)

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

const WUX_MAGIC0: u32 = 0x3058_5557;
const WUX_MAGIC1: u32 = 0x1099_D02E;
pub const DEFAULT_SECTOR_SIZE: u64 = 32768;

/// Compress the raw image at `in_path` into a `.wux` at `out_path`.
///
/// `progress(done, total)` is called periodically; returning `false` requests
/// cancellation, in which case the partial output is deleted and
/// `Err("__cancelled__")` is returned. Progress is reported in bytes across two
/// phases (scan, then emit) against a synthetic total of `2 * input_size`.
pub fn compress<F: FnMut(u64, u64) -> bool>(
    in_path: &Path,
    out_path: &Path,
    sector_size: u64,
    mut progress: F,
) -> Result<(), String> {
    if sector_size == 0 || (sector_size & (sector_size - 1)) != 0 {
        return Err(format!("WUX: invalid sector_size={sector_size}"));
    }
    let ss = sector_size as usize;

    let total_bytes = std::fs::metadata(in_path)
        .map_err(|e| format!("Stat input: {e}"))?
        .len();
    if total_bytes == 0 {
        return Err("WUX: input image is empty".to_string());
    }
    let num_sectors = (total_bytes + sector_size - 1) / sector_size;
    let progress_total = total_bytes.saturating_mul(2);

    // ── Pass 1: scan + dedup ────────────────────────────────────────────────
    let mut src = File::open(in_path).map_err(|e| format!("Open input: {e}"))?;
    let mut index_table: Vec<u32> = Vec::with_capacity(num_sectors as usize);
    let mut first_offset: Vec<u64> = Vec::new();
    let mut seen: HashMap<[u8; 32], u32> = HashMap::new();
    let mut buf = vec![0u8; ss];
    let mut scanned: u64 = 0;

    for i in 0..num_sectors {
        let off = i * sector_size;
        // Read up to a full sector; zero-pad the tail of the final short block.
        let want = (total_bytes - off).min(sector_size) as usize;
        src.read_exact(&mut buf[..want])
            .map_err(|e| format!("Read block {i}: {e}"))?;
        if want < ss {
            for b in &mut buf[want..] {
                *b = 0;
            }
        }

        let key = *blake3::hash(&buf).as_bytes();
        let phys = match seen.get(&key) {
            Some(&p) => p,
            None => {
                let p = first_offset.len() as u32;
                seen.insert(key, p);
                first_offset.push(off);
                p
            }
        };
        index_table.push(phys);

        scanned += want as u64;
        if !progress(scanned, progress_total) {
            return Err("__cancelled__".to_string());
        }
    }

    let num_unique = first_offset.len() as u64;

    // ── Space check (now that the output size is known) ──────────────────────
    let index_end = 0x20u64 + num_sectors * 4;
    let data_start = (index_end + sector_size - 1) / sector_size * sector_size;
    let out_size = data_start + num_unique * sector_size;
    if let Some(avail) = crate::ps3::available_space(out_path) {
        if avail < out_size {
            return Err(format!(
                "Not enough free space: need {out_size} bytes, only {avail} available"
            ));
        }
    }

    // ── Write header + index table ───────────────────────────────────────────
    let out = File::create(out_path).map_err(|e| format!("Create output: {e}"))?;
    let mut w = BufWriter::with_capacity(16 << 20, out);

    let mut hdr = [0u8; 32];
    hdr[0..4].copy_from_slice(&WUX_MAGIC0.to_le_bytes());
    hdr[4..8].copy_from_slice(&WUX_MAGIC1.to_le_bytes());
    hdr[8..12].copy_from_slice(&(sector_size as u32).to_le_bytes());
    hdr[16..24].copy_from_slice(&total_bytes.to_le_bytes());
    w.write_all(&hdr).map_err(|e| format!("Write header: {e}"))?;

    {
        let mut raw = vec![0u8; index_table.len() * 4];
        for (i, &v) in index_table.iter().enumerate() {
            raw[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        w.write_all(&raw).map_err(|e| format!("Write index: {e}"))?;
    }

    // Pad from index_end up to data_start.
    let pad = (data_start - index_end) as usize;
    if pad > 0 {
        let zeros = vec![0u8; pad];
        w.write_all(&zeros).map_err(|e| format!("Write pad: {e}"))?;
    }

    // ── Pass 2: emit unique blocks in physical order ─────────────────────────
    let mut written: u64 = 0;
    for (phys, &off) in first_offset.iter().enumerate() {
        let want = (total_bytes - off).min(sector_size) as usize;
        src.seek(SeekFrom::Start(off))
            .map_err(|e| format!("Seek block {phys}: {e}"))?;
        src.read_exact(&mut buf[..want])
            .map_err(|e| format!("Re-read block {phys}: {e}"))?;
        if want < ss {
            for b in &mut buf[want..] {
                *b = 0;
            }
        }
        w.write_all(&buf).map_err(|e| format!("Write block {phys}: {e}"))?;

        written += sector_size;
        if !progress(total_bytes + written.min(total_bytes), progress_total) {
            drop(w);
            let _ = std::fs::remove_file(out_path);
            return Err("__cancelled__".to_string());
        }
    }

    w.flush().map_err(|e| format!("Flush output: {e}"))?;
    let _ = progress(progress_total, progress_total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-contained round-trip on synthetic data: build a raw image with
    /// duplicate blocks and a short final block, compress it, read it back
    /// through WuxReader, and confirm exact reproduction + that dedup happened.
    #[test]
    fn synthetic_roundtrip_and_dedup() {
        let ss: u64 = 4096; // small power-of-2 sector for the test
        // 5 full sectors: A, B, A, zero, A  (A repeats 3×, so 3 unique full blocks),
        // plus a short 100-byte tail (a 4th unique block, zero-padded).
        let block_a: Vec<u8> = (0..ss).map(|i| (i % 251) as u8).collect();
        let block_b: Vec<u8> = (0..ss).map(|i| (i.wrapping_mul(7) % 251) as u8).collect();
        let block_z: Vec<u8> = vec![0u8; ss as usize];
        let tail: Vec<u8> = vec![0xAB; 100];

        let mut raw = Vec::new();
        raw.extend_from_slice(&block_a);
        raw.extend_from_slice(&block_b);
        raw.extend_from_slice(&block_a);
        raw.extend_from_slice(&block_z);
        raw.extend_from_slice(&block_a);
        raw.extend_from_slice(&tail);

        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let src = dir.join(format!("wux_rt_src_{pid}.bin"));
        let out = dir.join(format!("wux_rt_out_{pid}.wux"));
        std::fs::write(&src, &raw).unwrap();

        compress(&src, &out, ss, |_, _| true).unwrap();

        // 4 unique blocks (A, B, zero, tail-padded) → output must be smaller.
        let out_len = std::fs::metadata(&out).unwrap().len();
        assert!(out_len < raw.len() as u64, "dedup should shrink output");

        // Round-trip via the reader.
        let mut rdr = crate::wux_reader::WuxReader::open(&out).unwrap();
        let mut got = Vec::new();
        rdr.read_to_end(&mut got).unwrap();
        assert_eq!(got, raw, "round-trip mismatch");

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
    }

    /// Round-trip: compress a raw file, read it back through the reader, and
    /// confirm the bytes match. Set WUX_SRC (raw .wud/.iso) and WUX_OUT.
    #[test]
    #[ignore]
    fn compress_then_roundtrip() {
        let src = std::env::var("WUX_SRC").unwrap();
        let out = std::env::var("WUX_OUT").unwrap();
        compress(Path::new(&src), Path::new(&out), DEFAULT_SECTOR_SIZE, |_, _| true).unwrap();

        let mut orig = File::open(&src).unwrap();
        let mut rdr = crate::wux_reader::WuxReader::open(Path::new(&out)).unwrap();
        let mut a = vec![0u8; 8 << 20];
        let mut b = vec![0u8; 8 << 20];
        loop {
            let na = orig.read(&mut a).unwrap();
            if na == 0 {
                break;
            }
            let mut got = 0;
            while got < na {
                let n = rdr.read(&mut b[got..na]).unwrap();
                assert!(n > 0, "reader EOF before source");
                got += n;
            }
            assert_eq!(&a[..na], &b[..na], "round-trip mismatch");
        }
    }
}
