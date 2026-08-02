// GCZ (Dolphin compressed GameCube/Wii disc image) reader.
//
// GCZ splits the disc into fixed-size blocks and deflates each one. A block
// that failed to shrink is stored verbatim, flagged by the top bit of its
// pointer — so the pointer table is both an offset table and a per-block
// "is it compressed?" answer.
//
// File layout (all multi-byte integers little-endian):
//   Header (32 bytes):
//     [0x00] u32  magic = 0xB10BC001
//     [0x04] u32  sub_type — 0 = GameCube, 1 = Wii
//     [0x08] u64  compressed_data_size
//     [0x10] u64  data_size — uncompressed disc size
//     [0x18] u32  block_size
//     [0x1C] u32  num_blocks
//
//   Block pointer table at 0x20: num_blocks * u64.
//     Bit 63 set  = block is stored uncompressed.
//     Bits 0..62  = offset of the block within the data section.
//   Block hash table follows: num_blocks * u32 (Adler-32, not verified here).
//   Data section starts immediately after the hash table.
//
// A block's compressed length is the gap to the next pointer, and the last
// block runs to compressed_data_size — the header is the only record of where
// the data ends, since blocks carry no length of their own.
//
// Layout derived from SabreTools.Serialization (MIT, Copyright (c) 2018-2026
// Matt Nadareski), GCZ.cs.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use flate2::read::ZlibDecoder;

const GCZ_MAGIC: u32 = 0xB10B_C001;
const UNCOMPRESSED_FLAG: u64 = 1 << 63;
/// Same ceiling SabreTools applies: a table larger than this is a malformed or
/// hostile file rather than a real disc.
const MAX_BLOCKS: u32 = 0x10_0000;

pub struct GczReader {
    file: File,
    block_size: u64,
    total_bytes: u64,
    compressed_size: u64,
    pointers: Vec<u64>,
    data_start: u64,
    cache: Option<(u64, Vec<u8>)>,
    pos: u64,
}

impl GczReader {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut f = File::open(path).map_err(|e| format!("Cannot open GCZ: {e}"))?;

        let mut hdr = [0u8; 32];
        f.read_exact(&mut hdr).map_err(|e| format!("GCZ header read: {e}"))?;
        if u32::from_le_bytes(hdr[0..4].try_into().unwrap()) != GCZ_MAGIC {
            return Err("Not a GCZ file".to_string());
        }

        let compressed_size = u64::from_le_bytes(hdr[8..16].try_into().unwrap());
        let total_bytes = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
        let block_size = u32::from_le_bytes(hdr[24..28].try_into().unwrap()) as u64;
        let num_blocks = u32::from_le_bytes(hdr[28..32].try_into().unwrap());

        if block_size == 0 {
            return Err("GCZ: invalid block_size=0".to_string());
        }
        if num_blocks == 0 || num_blocks > MAX_BLOCKS {
            return Err(format!("GCZ: implausible block count {num_blocks}"));
        }

        let mut table = vec![0u8; num_blocks as usize * 8];
        f.read_exact(&mut table).map_err(|e| format!("GCZ block table: {e}"))?;
        let pointers: Vec<u64> = table
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect();

        // The Adler-32 table is skipped rather than read: block integrity is the
        // disc's problem, and reading it would cost a second pass over the file.
        let data_start = 32 + num_blocks as u64 * 8 + num_blocks as u64 * 4;

        Ok(GczReader {
            file: f,
            block_size,
            total_bytes,
            compressed_size,
            pointers,
            data_start,
            cache: None,
            pos: 0,
        })
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    fn load_block(&mut self, idx: u64) -> io::Result<()> {
        if self.cache.as_ref().is_some_and(|(i, _)| *i == idx) {
            return Ok(());
        }
        let i = idx as usize;
        let ptr = self.pointers[i];
        let plain = ptr & UNCOMPRESSED_FLAG != 0;
        let offset = ptr & !UNCOMPRESSED_FLAG;

        // A block runs until the next one starts; the last runs to the end of
        // the data section.
        let next = self
            .pointers
            .get(i + 1)
            .map(|p| p & !UNCOMPRESSED_FLAG)
            .unwrap_or(self.compressed_size);
        let len = next.saturating_sub(offset) as usize;
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GCZ: block {idx} has zero length"),
            ));
        }

        self.file.seek(SeekFrom::Start(self.data_start + offset))?;
        let mut comp = vec![0u8; len];
        self.file.read_exact(&mut comp)?;

        let block = if plain {
            comp
        } else {
            let mut out = Vec::with_capacity(self.block_size as usize);
            ZlibDecoder::new(&comp[..])
                .read_to_end(&mut out)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            out
        };

        self.cache = Some((idx, block));
        Ok(())
    }
}

impl Read for GczReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total_bytes || buf.is_empty() {
            return Ok(0);
        }
        let mut filled = 0usize;
        while filled < buf.len() && self.pos < self.total_bytes {
            let idx = self.pos / self.block_size;
            if idx as usize >= self.pointers.len() {
                break;
            }
            let off = (self.pos % self.block_size) as usize;
            self.load_block(idx)?;

            let block = &self.cache.as_ref().unwrap().1;
            let available = block.len().saturating_sub(off);
            let remaining = (self.total_bytes - self.pos) as usize;
            let n = (buf.len() - filled).min(available).min(remaining);
            if n == 0 {
                break;
            }
            buf[filled..filled + n].copy_from_slice(&block[off..off + n]);
            filled += n;
            self.pos += n as u64;
        }
        Ok(filled)
    }
}

impl Seek for GczReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(n) => self.total_bytes as i64 + n,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "GCZ: seek before start of image",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    // Build a GCZ whose blocks alternate compressed and stored, so both paths
    // through load_block are exercised by a single read.
    fn synth(dir: &Path, block_size: usize, blocks: usize) -> (std::path::PathBuf, Vec<u8>) {
        let mut plain = Vec::new();
        for b in 0..blocks {
            // Compressible for even blocks, incompressible-ish for odd ones.
            if b % 2 == 0 {
                plain.extend(std::iter::repeat_n(b as u8, block_size));
            } else {
                plain.extend((0..block_size).map(|i| (i * 7 + b) as u8));
            }
        }

        let mut data = Vec::new();
        let mut pointers = Vec::new();
        for b in 0..blocks {
            let src = &plain[b * block_size..(b + 1) * block_size];
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(src).unwrap();
            let comp = enc.finish().unwrap();
            if comp.len() < src.len() {
                pointers.push(data.len() as u64);
                data.extend_from_slice(&comp);
            } else {
                pointers.push(data.len() as u64 | UNCOMPRESSED_FLAG);
                data.extend_from_slice(src);
            }
        }

        let mut out = Vec::new();
        out.extend_from_slice(&GCZ_MAGIC.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // sub_type: GameCube
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&(plain.len() as u64).to_le_bytes());
        out.extend_from_slice(&(block_size as u32).to_le_bytes());
        out.extend_from_slice(&(blocks as u32).to_le_bytes());
        for p in &pointers {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out.extend_from_slice(&vec![0u8; blocks * 4]); // hash table, unread
        out.extend_from_slice(&data);

        let p = dir.join("test.gcz");
        std::fs::write(&p, out).unwrap();
        (p, plain)
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dx_gcz_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_back_exactly_what_was_compressed() {
        let d = scratch("roundtrip");
        let (path, plain) = synth(&d, 4096, 6);
        let mut r = GczReader::open(&path).unwrap();
        assert_eq!(r.total_bytes(), plain.len() as u64);

        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, plain, "sequential read must match the source image");
        let _ = std::fs::remove_dir_all(&d);
    }

    // The filesystem layer seeks to arbitrary offsets, so reads that start
    // mid-block and span several blocks are the normal case, not an edge case.
    #[test]
    fn random_access_crosses_block_boundaries() {
        let d = scratch("seek");
        let (path, plain) = synth(&d, 1024, 8);
        let mut r = GczReader::open(&path).unwrap();

        for (start, len) in [(0usize, 10usize), (1000, 100), (2047, 2050), (5000, 3192)] {
            r.seek(SeekFrom::Start(start as u64)).unwrap();
            let mut buf = vec![0u8; len];
            r.read_exact(&mut buf).unwrap();
            assert_eq!(buf, &plain[start..start + len], "read at {start}+{len}");
        }

        // Reading past the end stops rather than erroring.
        r.seek(SeekFrom::End(-4)).unwrap();
        let mut tail = Vec::new();
        r.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, &plain[plain.len() - 4..]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_files_that_are_not_gcz() {
        let d = scratch("reject");
        let p = d.join("nope.gcz");
        std::fs::write(&p, vec![0u8; 64]).unwrap();
        let err = GczReader::open(&p).err().expect("should reject");
        assert!(err.contains("Not a GCZ"), "{err}");

        // Truncated header.
        let p2 = d.join("short.gcz");
        std::fs::write(&p2, GCZ_MAGIC.to_le_bytes()).unwrap();
        assert!(GczReader::open(&p2).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
