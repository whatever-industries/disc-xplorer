//! Whole-image container conversion.
//!
//! Every conversion here is a stream copy between wrappers: the disc contents
//! come out byte-identical, only the container differs. That is what makes a
//! generic converter possible at all. Each supported input already has a reader
//! that presents the disc as a flat byte stream, so "convert" is a copy from one
//! of those into a writer for the target container.
//!
//! Deliberately not here: anything that would change the data. Rebuilding a
//! multi-track CD into a single .iso would silently drop the audio tracks, and
//! transcoding between CD sector modes would lose the subheaders. Those need
//! their own code paths and their own warnings, not a copy loop.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The error text the job runner recognises as "the user pressed Cancel", as
/// opposed to a real failure worth showing.
pub const CANCELLED: &str = "__cancelled__";

/// Copy buffer. Large because these are multi-gigabyte sequential reads off
/// spinning disks as often as not, and because it stays a whole number of
/// 2048-byte sectors, which the LBA-addressed readers rely on.
const CHUNK: usize = 16 << 20;

fn cancelled(out: &Path, writer: Option<BufWriter<File>>) -> String {
    drop(writer);
    let _ = std::fs::remove_file(out);
    CANCELLED.to_string()
}

/// Write `reader` out as a flat image: no compression, no container.
///
/// This covers every "decompress to ISO" conversion, and also ECM to BIN, since
/// in both cases the target format is just the bytes with nothing wrapped round
/// them. `total` is the uncompressed length, known up front from the source
/// header, so progress is real rather than estimated.
pub fn to_raw<F: FnMut(u64, u64)>(
    reader: &mut dyn Read,
    total: u64,
    out: &Path,
    cancel: &Arc<AtomicBool>,
    mut progress: F,
) -> Result<(), String> {
    let mut writer = BufWriter::with_capacity(
        CHUNK,
        File::create(out).map_err(|e| format!("Create output: {e}"))?,
    );
    let mut buf = vec![0u8; CHUNK];
    let (mut done, mut last) = (0u64, 0u64);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Read: {e}"))?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).map_err(|e| format!("Write: {e}"))?;
        done += n as u64;
        if cancel.load(Ordering::SeqCst) {
            return Err(cancelled(out, Some(writer)));
        }
        // Report at most ~100 times over the run, plus the final byte.
        if done == total || done - last >= total / 100 + 1 {
            last = done;
            progress(done, total);
        }
    }
    writer.flush().map_err(|e| format!("Flush: {e}"))?;
    progress(total, total);
    Ok(())
}

// ── CISO (compressed ISO) writer ─────────────────────────────────────────────
//
// Mirrors the reader in lib.rs: a 24-byte header, an index of (blocks + 1)
// little-endian u32 offsets, then the blocks themselves. Bit 31 of an index
// entry marks a block stored uncompressed; the low 31 bits are the file offset
// shifted right by `align`.

const CSO_MAGIC: &[u8; 4] = b"CISO";
const CSO_HEADER_SIZE: u32 = 24;

/// Block size to write. One 2048-byte logical sector per block is what PSP
/// tooling produces and what every CSO reader handles.
pub const CSO_BLOCK_SIZE: u32 = 2048;

/// Smallest shift that keeps every block offset inside the 31 bits an index
/// entry has room for.
///
/// A CSO is never larger than its source, so sizing this from the input length
/// is always safe, and it stays at 0 (byte-exact offsets, no padding) for
/// anything under 2 GB, which is every PSP UMD and most CDs.
fn align_for(total: u64) -> u8 {
    let mut align = 0u8;
    while (total >> align) >= 0x8000_0000 {
        align += 1;
    }
    align
}

/// Compress `reader` into a CISO at `out`.
///
/// Blocks that deflate to no smaller than they started are stored plain, which
/// is both smaller and faster to read back than a compressed block that gained
/// nothing. Already-compressed game data hits that case often.
pub fn to_cso<F: FnMut(u64, u64)>(
    reader: &mut dyn Read,
    total: u64,
    out: &Path,
    cancel: &Arc<AtomicBool>,
    mut progress: F,
) -> Result<(), String> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;

    if total == 0 {
        return Err("Source image is empty".to_string());
    }
    let bs = CSO_BLOCK_SIZE as u64;
    let num_blocks = total.div_ceil(bs);
    let align = align_for(total);

    let mut header = [0u8; CSO_HEADER_SIZE as usize];
    header[0..4].copy_from_slice(CSO_MAGIC);
    header[4..8].copy_from_slice(&CSO_HEADER_SIZE.to_le_bytes());
    header[8..16].copy_from_slice(&total.to_le_bytes());
    header[16..20].copy_from_slice(&CSO_BLOCK_SIZE.to_le_bytes());
    header[20] = 1; // version
    header[21] = align;

    let mut writer = BufWriter::with_capacity(
        CHUNK,
        File::create(out).map_err(|e| format!("Create output: {e}"))?,
    );
    writer.write_all(&header).map_err(|e| format!("Write: {e}"))?;

    // The index cannot be filled in until every block has been written, so
    // reserve its space now and seek back at the end.
    let index_len = (num_blocks + 1) as usize * 4;
    writer
        .write_all(&vec![0u8; index_len])
        .map_err(|e| format!("Write: {e}"))?;

    let mut index: Vec<u32> = Vec::with_capacity(num_blocks as usize + 1);
    let mut offset = CSO_HEADER_SIZE as u64 + index_len as u64;
    let mut block = vec![0u8; bs as usize];
    let (mut done, mut last) = (0u64, 0u64);

    for _ in 0..num_blocks {
        // The final block of an image whose length is not a multiple of the
        // block size is short; pad it so the reader always gets a full block.
        let want = (total - done).min(bs) as usize;
        reader
            .read_exact(&mut block[..want])
            .map_err(|e| format!("Read: {e}"))?;
        if want < block.len() {
            block[want..].fill(0);
        }

        // Pad to the alignment boundary before recording this block's offset.
        if align > 0 {
            let pad = (offset.next_multiple_of(1u64 << align)) - offset;
            if pad > 0 {
                writer
                    .write_all(&vec![0u8; pad as usize])
                    .map_err(|e| format!("Write: {e}"))?;
                offset += pad;
            }
        }

        let mut enc = DeflateEncoder::new(Vec::with_capacity(bs as usize), Compression::default());
        enc.write_all(&block).map_err(|e| format!("Deflate: {e}"))?;
        let packed = enc.finish().map_err(|e| format!("Deflate: {e}"))?;

        let entry = (offset >> align) as u32;
        if packed.len() >= block.len() {
            index.push(entry | 0x8000_0000);
            writer.write_all(&block).map_err(|e| format!("Write: {e}"))?;
            offset += block.len() as u64;
        } else {
            index.push(entry);
            writer.write_all(&packed).map_err(|e| format!("Write: {e}"))?;
            offset += packed.len() as u64;
        }

        done += want as u64;
        if cancel.load(Ordering::SeqCst) {
            return Err(cancelled(out, Some(writer)));
        }
        if done == total || done - last >= total / 100 + 1 {
            last = done;
            progress(done, total);
        }
    }
    // The reader derives each block's length from the following entry, so the
    // index carries one extra entry marking the end of the last block.
    index.push((offset >> align) as u32);

    let mut file = writer.into_inner().map_err(|e| format!("Flush: {e}"))?;
    file.seek(SeekFrom::Start(CSO_HEADER_SIZE as u64))
        .map_err(|e| format!("Seek: {e}"))?;
    let mut raw = Vec::with_capacity(index_len);
    for e in &index {
        raw.extend_from_slice(&e.to_le_bytes());
    }
    file.write_all(&raw).map_err(|e| format!("Write index: {e}"))?;
    file.flush().map_err(|e| format!("Flush: {e}"))?;
    progress(total, total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_stays_zero_below_two_gigabytes() {
        assert_eq!(align_for(0), 0);
        assert_eq!(align_for(700 * 1024 * 1024), 0);
        assert_eq!(align_for(0x7FFF_FFFF), 0);
    }

    #[test]
    fn align_grows_past_the_index_limit() {
        // A DVD cannot address its own tail with byte-exact offsets.
        assert_eq!(align_for(0x8000_0000), 1);
        assert_eq!(align_for(4_700_000_000), 2);
        assert_eq!(align_for(8_500_000_000), 2);
        // Blu-ray sized, to check the loop keeps going rather than saturating.
        assert_eq!(align_for(50_000_000_000), 5);
    }

    /// The point of the writer is that our own reader can read it back, so the
    /// test asserts exactly that, over data that exercises both storage paths:
    /// a compressible run, an incompressible one, and a short final block.
    #[test]
    fn cso_round_trips_through_the_reader() {
        let mut src = Vec::new();
        src.extend(std::iter::repeat(0u8).take(2048)); // deflates to nothing
        for i in 0..2048u32 {
            src.push((i.wrapping_mul(2654435761) >> 13) as u8); // does not
        }
        src.extend_from_slice(b"tail"); // short final block

        let dir = std::env::temp_dir().join("dx_cso_roundtrip");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("out.cso");
        let total = src.len() as u64;
        let cancel = Arc::new(AtomicBool::new(false));
        to_cso(&mut &src[..], total, &out, &cancel, |_, _| {}).unwrap();

        // Smaller than the source, so the compressed path really was taken.
        assert!(std::fs::metadata(&out).unwrap().len() < total);

        let mut reader = crate::CsoReader::open(&out).unwrap();
        let mut got = Vec::new();
        for lba in 0..total.div_ceil(2048) {
            let mut buf = [0u8; 2048];
            let n = crate::ISO9660Reader::read_at(&mut reader, &mut buf, lba).unwrap();
            got.extend_from_slice(&buf[..n]);
        }
        got.truncate(src.len());
        assert_eq!(got, src);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn cancelling_removes_the_partial_output() {
        let dir = std::env::temp_dir().join("dx_cso_cancel");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("cancelled.iso");
        let src = vec![7u8; 4096];
        let cancel = Arc::new(AtomicBool::new(true));
        let err = to_raw(&mut &src[..], src.len() as u64, &out, &cancel, |_, _| {}).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(!out.exists(), "partial output should be deleted");
    }
}
