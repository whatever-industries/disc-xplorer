// WIA / RVZ (Dolphin compressed GameCube and Wii disc images) reader.
//
// The disc is described by a set of "raw data" entries — byte ranges of the
// disc — each cut into chunk_size pieces. Every piece is one "group", and a
// group entry says where its compressed bytes live in the file. RVZ adds two
// things on top of WIA: Zstandard as a compression method, and "packing", which
// replaces the disc's junk padding with a 68-byte PRNG seed instead of storing
// megabytes of pseudo-random bytes.
//
// Everything except the magic is big-endian.
//
//   Header1 (0x48 bytes):
//     [0x00] u32  magic — "WIA\1" or "RVZ\1"
//     [0x04] u32  version
//     [0x08] u32  version_compatible
//     [0x0C] u32  size of Header2
//     [0x10] 20   SHA-1 of Header1
//     [0x24] u64  uncompressed disc size
//     [0x2C] u64  size of this file
//     [0x34] 20   SHA-1 of Header2
//
//   Header2 (0xDC bytes, at 0x48):
//     [0x00] u32  disc type — 1 = GameCube, 2 = Wii
//     [0x04] u32  compression type
//     [0x08] i32  compression level
//     [0x0C] u32  chunk size
//     [0x10] 0x80 copy of the disc header
//     [0x90] u32  number of partition entries
//     [0x94] u32  partition entry size
//     [0x98] u64  partition entries offset
//     [0xA0] 20   SHA-1 of the partition entries
//     [0xB4] u32  number of raw data entries
//     [0xB8] u64  raw data entries offset
//     [0xC0] u32  raw data entries size   (compressed)
//     [0xC4] u32  number of group entries
//     [0xC8] u64  group entries offset
//     [0xD0] u32  group entries size      (compressed)
//     [0xD4] u8   compressor data size
//     [0xD5] 7    compressor data — LZMA properties, when LZMA is in use
//
// The raw data and group tables are themselves compressed with the file's own
// compression method; only the partition table is stored plain. (SabreTools's
// reader treats all three as plain, which is why it was not used as the source
// here — it can only read compression_type = NONE files.)
//
// Spec: Dolphin's docs/WiaAndRvz.md.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const WIA_MAGIC: &[u8; 4] = b"WIA\x01";
const RVZ_MAGIC: &[u8; 4] = b"RVZ\x01";
const HEADER2_MIN: usize = 0xDC;

const COMPRESSION_NONE: u32 = 0;
const COMPRESSION_PURGE: u32 = 1;
const COMPRESSION_BZIP2: u32 = 2;
const COMPRESSION_LZMA: u32 = 3;
const COMPRESSION_LZMA2: u32 = 4;
const COMPRESSION_ZSTD: u32 = 5;

/// A group whose compressed size clears this bit was stored uncompressed even
/// though the file declares a compression method (RVZ only).
const RVZ_COMPRESSED_FLAG: u32 = 0x8000_0000;

/// One half of a partition: a run of disc sectors and the groups holding them.
#[derive(Clone, Copy)]
struct PartitionSegment {
    sectors: u32,
    group_index: u32,
}

/// A Wii partition. Its two segments are contiguous, and together they are the
/// partition's decrypted contents.
struct Partition {
    segments: [PartitionSegment; 2],
}

struct RawDataEntry {
    disc_offset: u64,
    disc_size: u64,
    group_index: u32,
    group_count: u32,
}

struct GroupEntry {
    data_offset4: u32,
    /// Compressed size, with the RVZ flag already stripped.
    data_size: u32,
    compressed: bool,
    packed_size: u32,
}

pub struct WiaReader {
    file: File,
    compression: u32,
    compressor_data: Vec<u8>,
    chunk_size: u32,
    disc_size: u64,
    partitions: Vec<Partition>,
    /// The disc's first 0x80 bytes, which live in Header2 rather than in any
    /// group, because the first raw data entry deliberately starts after them.
    disc_head: [u8; 0x80],
    raw_data: Vec<RawDataEntry>,
    groups: Vec<GroupEntry>,
    cache: Option<(usize, Vec<u8>)>,
    pos: u64,
}

fn be16(d: &[u8], p: usize) -> u16 {
    u16::from_be_bytes(d[p..p + 2].try_into().unwrap())
}
fn be32(d: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(d[p..p + 4].try_into().unwrap())
}
fn be64(d: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(d[p..p + 8].try_into().unwrap())
}

/// Nintendo's lagged Fibonacci generator, used to regenerate the junk padding
/// that RVZ packing throws away. The odd `>> 18` in the output step is faithful
/// to the original implementation, not a typo.
struct JunkGenerator {
    buffer: [u32; 521],
    index: usize,
}

impl JunkGenerator {
    fn new(seed: &[u8]) -> Self {
        let mut buffer = [0u32; 521];
        for (i, word) in seed.chunks_exact(4).take(17).enumerate() {
            buffer[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 17..521 {
            buffer[i] = (buffer[i - 17] << 23) ^ (buffer[i - 16] >> 9) ^ buffer[i - 1];
        }
        let mut g = JunkGenerator { buffer, index: 521 };
        for _ in 0..4 {
            g.advance();
        }
        g.index = 0;
        g
    }

    fn advance(&mut self) {
        for i in 0..32 {
            self.buffer[i] ^= self.buffer[i + 521 - 32];
        }
        for i in 32..521 {
            self.buffer[i] ^= self.buffer[i - 32];
        }
    }

    fn fill(&mut self, out: &mut [u8]) {
        let mut written = 0;
        while written < out.len() {
            if self.index >= 521 {
                self.advance();
                self.index = 0;
            }
            let w = self.buffer[self.index];
            self.index += 1;
            for b in [(w >> 24) as u8, (w >> 18) as u8, (w >> 8) as u8, w as u8] {
                if written < out.len() {
                    out[written] = b;
                    written += 1;
                }
            }
        }
    }
}

fn decompress(method: u32, compressor_data: &[u8], input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    match method {
        COMPRESSION_NONE => Ok(input.to_vec()),
        COMPRESSION_ZSTD => zstd::bulk::decompress(input, expected.max(1))
            .map_err(|e| format!("WIA: zstd chunk failed: {e}")),
        COMPRESSION_BZIP2 => {
            let mut out = Vec::with_capacity(expected);
            bzip2_rs::DecoderReader::new(input)
                .read_to_end(&mut out)
                .map_err(|e| format!("WIA: bzip2 chunk failed: {e}"))?;
            Ok(out)
        }
        COMPRESSION_LZMA => {
            // WIA stores the 5 LZMA property bytes once in the header rather than
            // in front of every chunk, and never records an end marker — so the
            // 13-byte header the decoder expects has to be rebuilt here, with the
            // known output size standing in for the usual 0xFFFFFFFFFFFFFFFF.
            if compressor_data.len() < 5 {
                return Err("WIA: LZMA properties missing from the header".into());
            }
            let mut stream = Vec::with_capacity(13 + input.len());
            stream.extend_from_slice(&compressor_data[..5]);
            stream.extend_from_slice(&(expected as u64).to_le_bytes());
            stream.extend_from_slice(input);
            let mut out = Vec::with_capacity(expected);
            lzma_rs::lzma_decompress(&mut &stream[..], &mut out)
                .map_err(|e| format!("WIA: LZMA chunk failed: {e}"))?;
            Ok(out)
        }
        COMPRESSION_LZMA2 => {
            let mut out = Vec::with_capacity(expected);
            lzma_rs::lzma2_decompress(&mut &input[..], &mut out)
                .map_err(|e| format!("WIA: LZMA2 chunk failed: {e}"))?;
            Ok(out)
        }
        COMPRESSION_PURGE => purge_decode(input, expected),
        other => Err(format!("WIA: unsupported compression method {other}")),
    }
}

/// PURGE stores runs of non-zero data as (offset, size) segments and leaves
/// everything else zero, then ends with a SHA-1 of the result.
fn purge_decode(input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; expected];
    let mut p = 0usize;
    while p + 8 <= input.len().saturating_sub(20) {
        let offset = be32(input, p) as usize;
        let size = be32(input, p + 4) as usize;
        p += 8;
        if p + size > input.len() || offset + size > expected {
            return Err("WIA: PURGE segment runs outside the chunk".into());
        }
        out[offset..offset + size].copy_from_slice(&input[p..p + size]);
        p += size;
    }
    Ok(out)
}

impl WiaReader {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("Cannot open WIA/RVZ: {e}"))?;

        let mut h1 = [0u8; 0x48];
        file.read_exact(&mut h1).map_err(|e| format!("WIA header: {e}"))?;
        let is_rvz = match &h1[0..4] {
            m if m == RVZ_MAGIC => true,
            m if m == WIA_MAGIC => false,
            _ => return Err("Not a WIA or RVZ file".into()),
        };

        let header2_size = be32(&h1, 0x0C) as usize;
        let disc_size = be64(&h1, 0x24);
        if header2_size < HEADER2_MIN {
            return Err(format!("WIA: header2 is {header2_size} bytes, expected at least {HEADER2_MIN}"));
        }

        let mut h2 = vec![0u8; header2_size];
        file.read_exact(&mut h2).map_err(|e| format!("WIA header2: {e}"))?;

        let compression = be32(&h2, 0x04);
        let chunk_size = be32(&h2, 0x0C);
        if chunk_size == 0 {
            return Err("WIA: chunk size is zero".into());
        }

        let comp_data_len = (h2[0xD4] as usize).min(7);
        let compressor_data = h2[0xD5..0xD5 + comp_data_len].to_vec();

        // Wii images describe their partitions separately from the raw data
        // entries. The partition table is the one section stored uncompressed.
        let num_partitions = be32(&h2, 0x90) as usize;
        let partition_entry_size = be32(&h2, 0x94) as usize;
        let partition_offset = be64(&h2, 0x98);
        let mut partitions = Vec::new();
        if num_partitions > 0 && partition_entry_size >= 48 {
            let mut raw = vec![0u8; num_partitions * partition_entry_size];
            file.seek(SeekFrom::Start(partition_offset))
                .map_err(|e| format!("WIA partition table seek: {e}"))?;
            file.read_exact(&mut raw)
                .map_err(|e| format!("WIA partition table: {e}"))?;
            for i in 0..num_partitions {
                let e = &raw[i * partition_entry_size..];
                // 16 bytes of AES key, then the two segments.
                let seg = |n: usize| PartitionSegment {
                    sectors: be32(e, 16 + n * 16 + 4),
                    group_index: be32(e, 16 + n * 16 + 8),
                };
                partitions.push(Partition { segments: [seg(0), seg(1)] });
            }
        }

        let num_raw = be32(&h2, 0xB4) as usize;
        let raw_offset = be64(&h2, 0xB8);
        let raw_size = be32(&h2, 0xC0) as usize;
        let num_groups = be32(&h2, 0xC4) as usize;
        let group_offset = be64(&h2, 0xC8);
        let group_size = be32(&h2, 0xD0) as usize;

        // Both tables are compressed with the file's own method.
        let read_table = |f: &mut File, off: u64, size: usize, expected: usize, what: &str| -> Result<Vec<u8>, String> {
            if size == 0 || off == 0 {
                return Ok(Vec::new());
            }
            let mut raw = vec![0u8; size];
            f.seek(SeekFrom::Start(off)).map_err(|e| format!("WIA {what} seek: {e}"))?;
            f.read_exact(&mut raw).map_err(|e| format!("WIA {what}: {e}"))?;
            decompress(compression, &compressor_data, &raw, expected)
        };

        let raw_table = read_table(&mut file, raw_offset, raw_size, num_raw * 24, "raw data entries")?;
        let group_entry_size = if is_rvz { 12 } else { 8 };
        let group_table = read_table(&mut file, group_offset, group_size, num_groups * group_entry_size, "group entries")?;

        if raw_table.len() < num_raw * 24 {
            return Err("WIA: raw data table is shorter than its entry count".into());
        }
        if group_table.len() < num_groups * group_entry_size {
            return Err("WIA: group table is shorter than its entry count".into());
        }

        let mut raw_data: Vec<RawDataEntry> = (0..num_raw)
            .map(|i| {
                let p = i * 24;
                RawDataEntry {
                    disc_offset: be64(&raw_table, p),
                    disc_size: be64(&raw_table, p + 8),
                    group_index: be32(&raw_table, p + 16),
                    group_count: be32(&raw_table, p + 20),
                }
            })
            .collect();

        // The first entry is written starting at 0x80, because the disc's first
        // 0x80 bytes are kept in Header2 instead. The spec's advice is to round
        // the offset down to the previous multiple of 0x8000 and grow the size to
        // match, rather than treat it as a special case everywhere else.
        if let Some(first) = raw_data.first_mut() {
            let aligned = first.disc_offset & !0x7FFF;
            first.disc_size += first.disc_offset - aligned;
            first.disc_offset = aligned;
        }

        let mut disc_head = [0u8; 0x80];
        disc_head.copy_from_slice(&h2[0x10..0x90]);

        let groups: Vec<GroupEntry> = (0..num_groups)
            .map(|i| {
                let p = i * group_entry_size;
                let size_word = be32(&group_table, p + 4);
                if is_rvz {
                    GroupEntry {
                        data_offset4: be32(&group_table, p),
                        data_size: size_word & !RVZ_COMPRESSED_FLAG,
                        compressed: size_word & RVZ_COMPRESSED_FLAG != 0,
                        packed_size: be32(&group_table, p + 8),
                    }
                } else {
                    GroupEntry {
                        data_offset4: be32(&group_table, p),
                        data_size: size_word,
                        compressed: true,
                        packed_size: 0,
                    }
                }
            })
            .collect();

        Ok(WiaReader {
            file,
            compression,
            compressor_data,
            chunk_size,
            disc_size,
            disc_head,
            partitions,
            raw_data,
            groups,
            cache: None,
            pos: 0,
        })
    }

    pub fn total_bytes(&self) -> u64 {
        self.disc_size
    }

    /// Undo RVZ packing: a stream of (size, data) runs where a size with its top
    /// bit set means "regenerate this many junk bytes from the following seed"
    /// rather than "copy these bytes".
    fn unpack(&self, packed: &[u8], out_len: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(out_len);
        let mut p = 0usize;
        while p + 4 <= packed.len() && out.len() < out_len {
            let word = be32(packed, p);
            p += 4;
            let size = (word & 0x7FFF_FFFF) as usize;
            if word & 0x8000_0000 != 0 {
                if p + 68 > packed.len() {
                    return Err("WIA: RVZ junk seed runs past the end of the chunk".into());
                }
                let mut gen = JunkGenerator::new(&packed[p..p + 68]);
                p += 68;
                let mut junk = vec![0u8; size];
                gen.fill(&mut junk);
                out.extend_from_slice(&junk);
            } else {
                if p + size > packed.len() {
                    return Err("WIA: RVZ packed run runs past the end of the chunk".into());
                }
                out.extend_from_slice(&packed[p..p + size]);
                p += size;
            }
        }
        out.resize(out_len, 0);
        Ok(out)
    }

    fn load_group(&mut self, index: usize, decompressed_len: usize) -> Result<(), String> {
        if self.cache.as_ref().is_some_and(|(i, _)| *i == index) {
            return Ok(());
        }
        let g = self
            .groups
            .get(index)
            .ok_or_else(|| format!("WIA: group {index} is out of range"))?;
        let (offset, size, compressed, packed_size) =
            (g.data_offset4 as u64 * 4, g.data_size as usize, g.compressed, g.packed_size as usize);

        // A zero size means the whole chunk decompresses to zeroes.
        if size == 0 {
            self.cache = Some((index, vec![0u8; decompressed_len]));
            return Ok(());
        }

        let mut raw = vec![0u8; size];
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| format!("WIA seek: {e}"))?;
        self.file
            .read_exact(&mut raw)
            .map_err(|e| format!("WIA read: {e}"))?;

        // Packed groups decompress to packed_size first, then decode to the chunk.
        let target = if packed_size != 0 { packed_size } else { decompressed_len };
        let stage = if compressed {
            decompress(self.compression, &self.compressor_data, &raw, target)?
        } else {
            raw
        };
        let mut block = if packed_size != 0 {
            self.unpack(&stage, decompressed_len)?
        } else {
            stage
        };
        block.resize(decompressed_len, 0);

        self.cache = Some((index, block));
        Ok(())
    }

    /// Which group covers `offset`, and where within its chunk that offset falls.
    fn locate(&self, offset: u64) -> Option<(usize, usize, usize)> {
        let e = self
            .raw_data
            .iter()
            .find(|e| offset >= e.disc_offset && offset < e.disc_offset + e.disc_size)?;
        let within = offset - e.disc_offset;
        let chunk = within / self.chunk_size as u64;
        if chunk >= e.group_count as u64 {
            return None;
        }
        // The last chunk of an entry is short when the range is not a whole
        // number of chunks.
        let chunk_start = chunk * self.chunk_size as u64;
        let len = (e.disc_size - chunk_start).min(self.chunk_size as u64) as usize;
        Some((
            e.group_index as usize + chunk as usize,
            (within % self.chunk_size as u64) as usize,
            len,
        ))
    }
}

/// A Wii disc sector, and the part of it that survives hash removal.
const WII_SECTOR: usize = 0x8000;
const WII_SECTOR_DATA: usize = 0x7C00;
/// One hash exception: a 16-bit offset and a 20-byte SHA-1.
const EXCEPTION_SIZE: usize = 22;

impl WiaReader {
    /// How many partitions the image describes.
    pub fn partition_count(&self) -> usize {
        self.partitions.len()
    }

    /// Decrypted length of a partition, in bytes.
    pub fn partition_len(&self, index: usize) -> u64 {
        self.partitions.get(index).map_or(0, |p| {
            p.segments.iter().map(|s| s.sectors as u64).sum::<u64>() * WII_SECTOR_DATA as u64
        })
    }

    /// Read from a partition's decrypted contents.
    ///
    /// Wii partition data is stored already decrypted and with the 0x400-byte
    /// hash block stripped from every 0x8000 sector, so browsing needs neither
    /// the key nor the hashes — only reassembly. Each group is prefixed by hash
    /// exception lists, which matter for rebuilding an exact disc image but not
    /// for reading files, so they are skipped.
    fn read_partition_at(&mut self, index: usize, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let Some(part) = self.partitions.get(index) else { return Ok(0) };
        let total = self.partition_len(index);
        if offset >= total || buf.is_empty() {
            return Ok(0);
        }
        let sectors_per_group = (self.chunk_size as usize / WII_SECTOR).max(1);
        let group_payload = sectors_per_group * WII_SECTOR_DATA;
        let segments = part.segments;

        let mut filled = 0usize;
        let mut pos = offset;
        while filled < buf.len() && pos < total {
            let sector = (pos / WII_SECTOR_DATA as u64) as u32;
            let within = (pos % WII_SECTOR_DATA as u64) as usize;

            // Which of the two segments holds this sector.
            let (seg, local) = if sector < segments[0].sectors {
                (segments[0], sector)
            } else {
                (segments[1], sector - segments[0].sectors)
            };
            let group = seg.group_index as usize + local as usize / sectors_per_group;
            let offset_in_group =
                (local as usize % sectors_per_group) * WII_SECTOR_DATA + within;

            self.load_partition_group(group, group_payload)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let block = &self.cache.as_ref().unwrap().1;
            let available = block.len().saturating_sub(offset_in_group);
            let remaining = (total - pos) as usize;
            let n = (buf.len() - filled).min(available).min(remaining);
            if n == 0 {
                break;
            }
            buf[filled..filled + n].copy_from_slice(&block[offset_in_group..offset_in_group + n]);
            filled += n;
            pos += n as u64;
        }
        Ok(filled)
    }

    /// Decompress a partition group and strip its exception lists.
    fn load_partition_group(&mut self, index: usize, payload: usize) -> Result<(), String> {
        // The cache is shared with raw-data reads; key it on the group index.
        if self.cache.as_ref().is_some_and(|(i, _)| *i == index) {
            return Ok(());
        }
        let g = self
            .groups
            .get(index)
            .ok_or_else(|| format!("WIA: group {index} is out of range"))?;
        let (offset, size, compressed, packed_size) =
            (g.data_offset4 as u64 * 4, g.data_size as usize, g.compressed, g.packed_size as usize);

        if size == 0 {
            self.cache = Some((index, vec![0u8; payload]));
            return Ok(());
        }

        let mut raw = vec![0u8; size];
        self.file.seek(SeekFrom::Start(offset)).map_err(|e| format!("WIA seek: {e}"))?;
        self.file.read_exact(&mut raw).map_err(|e| format!("WIA read: {e}"))?;

        // Exception lists sit ahead of the payload, so a group decompresses to the
        // payload plus however many exceptions it carries. A list can hold 65535
        // of them at 22 bytes each, so allow the full 2 MiB rather than guessing
        // small — this is an upper bound for the buffer, not the real size.
        // A list can hold 65535 exceptions at 22 bytes each, so allow for the
        // worst case rather than guessing: this bounds the buffer, it is not the
        // real size.
        let slack = 2 + 65_535 * EXCEPTION_SIZE;
        // rvz_packed_size measures the packed *data*, but the exception lists sit
        // in front of it inside the same compressed blob, so the whole group
        // decompresses to lists + packed data.
        let target = if packed_size != 0 { packed_size + slack } else { payload + slack };
        let stage = if compressed {
            decompress(self.compression, &self.compressor_data, &raw, target)?
        } else {
            raw
        };

        // One list per 2 MiB of group, at least one. They record hashes that
        // could not be recomputed — needed to rebuild an exact disc image, but
        // not to read files out of one.
        let lists = (self.chunk_size as usize / 0x20_0000).max(1);
        let mut at = 0usize;
        for _ in 0..lists {
            if at + 2 > stage.len() {
                return Err("WIA: exception list runs past the end of the group".into());
            }
            let count = be16(&stage, at) as usize;
            at += 2 + count * EXCEPTION_SIZE;
        }
        if at > stage.len() {
            return Err("WIA: exception list is longer than the group".into());
        }

        // Only now can the packing be undone, since it covers the data alone.
        let body = &stage[at..];
        let mut block = if packed_size != 0 {
            self.unpack(body, payload)?
        } else {
            body.to_vec()
        };
        block.resize(payload, 0);
        self.cache = Some((index, block));
        Ok(())
    }
}

/// A reader over one partition's decrypted contents.
pub struct WiaPartitionReader {
    inner: WiaReader,
    index: usize,
    pos: u64,
}

impl WiaPartitionReader {
    pub fn new(inner: WiaReader, index: usize) -> Self {
        WiaPartitionReader { inner, index, pos: 0 }
    }
    pub fn len(&self) -> u64 {
        self.inner.partition_len(self.index)
    }
}

impl Read for WiaPartitionReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read_partition_at(self.index, self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for WiaPartitionReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(n) => self.len() as i64 + n,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

impl Read for WiaReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.disc_size || buf.is_empty() {
            return Ok(0);
        }
        let buf_start = self.pos;
        let mut filled = 0usize;
        while filled < buf.len() && self.pos < self.disc_size {
            let Some((group, within, chunk_len)) = self.locate(self.pos) else {
                // Disc ranges not covered by any raw data entry read as zeroes,
                // which is how WIA represents areas it did not store.
                let n = buf.len() - filled;
                buf[filled..filled + n].fill(0);
                filled += n;
                self.pos += n as u64;
                continue;
            };
            self.load_group(group, chunk_len)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let block = &self.cache.as_ref().unwrap().1;
            let available = block.len().saturating_sub(within);
            let remaining = (self.disc_size - self.pos) as usize;
            let n = (buf.len() - filled).min(available).min(remaining);
            if n == 0 {
                break;
            }
            buf[filled..filled + n].copy_from_slice(&block[within..within + n]);
            filled += n;
            self.pos += n as u64;
        }

        // Overlay the real disc header, which no group carries.
        let start = buf_start;
        if start < 0x80 {
            let overlap = ((start + filled as u64).min(0x80) - start) as usize;
            buf[..overlap].copy_from_slice(&self.disc_head[start as usize..start as usize + overlap]);
        }
        Ok(filled)
    }
}

impl Seek for WiaReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(n) => self.disc_size as i64 + n,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WIA: seek before start of image",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_fills_the_gaps_with_zeroes() {
        let mut input = Vec::new();
        input.extend_from_slice(&4u32.to_be_bytes()); // offset
        input.extend_from_slice(&3u32.to_be_bytes()); // size
        input.extend_from_slice(b"abc");
        input.extend_from_slice(&[0u8; 20]); // trailing SHA-1
        let out = purge_decode(&input, 10).unwrap();
        assert_eq!(out, vec![0, 0, 0, 0, b'a', b'b', b'c', 0, 0, 0]);
    }

    #[test]
    fn purge_rejects_segments_outside_the_chunk() {
        let mut input = Vec::new();
        input.extend_from_slice(&8u32.to_be_bytes());
        input.extend_from_slice(&8u32.to_be_bytes());
        input.extend_from_slice(&[1u8; 8]);
        input.extend_from_slice(&[0u8; 20]);
        assert!(purge_decode(&input, 10).is_err());
    }

    // The generator is deterministic from its seed, so the same seed must always
    // give the same bytes, and it must keep producing past one buffer length
    // (521 words = 2084 bytes) where the state has to advance again.
    #[test]
    fn junk_generator_is_deterministic_and_continues_past_a_buffer() {
        let seed: Vec<u8> = (0..68u8).collect();
        let mut a = vec![0u8; 5000];
        let mut b = vec![0u8; 5000];
        JunkGenerator::new(&seed).fill(&mut a);
        JunkGenerator::new(&seed).fill(&mut b);
        assert_eq!(a, b, "same seed must give the same stream");
        assert!(a[2084..].iter().any(|&x| x != 0), "output continues past one buffer");

        let other: Vec<u8> = (0..68u8).map(|i| i.wrapping_add(1)).collect();
        let mut c = vec![0u8; 64];
        JunkGenerator::new(&other).fill(&mut c);
        assert_ne!(&a[..64], &c[..], "a different seed gives a different stream");
    }

    // Exception lists are skipped by walking their counts, and getting that
    // arithmetic wrong shifts every byte of the payload.
    #[test]
    fn exception_lists_are_stepped_over_by_count() {
        // One list holding two exceptions, then the payload.
        let mut group = Vec::new();
        group.extend_from_slice(&2u16.to_be_bytes());
        group.extend_from_slice(&[0xAA; 2 * EXCEPTION_SIZE]);
        group.extend_from_slice(b"PAYLOAD");

        let lists = 1;
        let mut at = 0usize;
        for _ in 0..lists {
            let count = be16(&group, at) as usize;
            at += 2 + count * EXCEPTION_SIZE;
        }
        assert_eq!(&group[at..], b"PAYLOAD");
        assert_eq!(at, 2 + 2 * EXCEPTION_SIZE);
    }

    // A Wii sector keeps 0x7C00 of its 0x8000 bytes once the hash block is gone,
    // so a group holds that much per sector rather than the full chunk.
    #[test]
    fn group_payload_excludes_the_hash_blocks() {
        let chunk = 0x20000usize; // 128 KiB, what both test images use
        let sectors = chunk / WII_SECTOR;
        assert_eq!(sectors, 4);
        assert_eq!(sectors * WII_SECTOR_DATA, 126_976);
        assert!(sectors * WII_SECTOR_DATA < chunk, "hashes are removed, so it is smaller");
    }

    #[test]
    fn rejects_files_that_are_not_wia_or_rvz() {
        let d = std::env::temp_dir().join("dx_wia_reject");
        let _ = std::fs::create_dir_all(&d);
        let p = d.join("nope.rvz");
        std::fs::write(&p, vec![0u8; 200]).unwrap();
        let err = WiaReader::open(&p).err().expect("should reject");
        assert!(err.contains("Not a WIA or RVZ"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
