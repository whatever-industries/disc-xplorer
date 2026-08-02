// ZArchive (.wua — Wii U Archive) reader.
//
// Cemu's container for a Wii U title: the whole directory tree of a decrypted
// title, with the file data cut into fixed 64 KB blocks and each block
// Zstandard-compressed. It completes our Wii U coverage — WUX/WUD are the disc
// images, .wua is the unpacked title in archive form.
//
// Everything is big-endian. Sections are located by a fixed-size footer at the
// very end of the file:
//
//   Footer (144 bytes, at EOF):
//     6 * { u64 offset, u64 size }  — compressed data, offset records,
//                                     name table, file tree, meta directory,
//                                     meta data
//     [0x60] 32 bytes  SHA-256 integrity hash
//     [0x80] u64       total archive size
//     [0x88] u32       version = 61 BF 3A 01
//     [0x8C] u32       magic   = 16 9F 52 D6
//
//   Offset records (40 bytes each) cover 16 blocks apiece: a base offset into
//   the data section, then 16 stored sizes. Each size is encoded one less than
//   the real length, and a block whose stored length equals the block size is
//   held uncompressed rather than Zstandard-compressed.
//
//   The file tree is a flat array of 16-byte nodes. A node is a file when bit
//   31 of its first word is set; the low bits are a byte offset into the name
//   table. Directories point at a contiguous run of children, so the tree is
//   walked by index rather than by following pointers.
//
// Layout derived from SabreTools.Serialization (MIT, Copyright (c) 2018-2026
// Matt Nadareski), ZArchive.cs and ZArchive.Extraction.cs.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::DiscEntry;

const FOOTER_SIZE: u64 = 144;
const MAGIC: [u8; 4] = [0x16, 0x9F, 0x52, 0xD6];
const VERSION_1: [u8; 4] = [0x61, 0xBF, 0x3A, 0x01];
const BLOCK_SIZE: usize = 64 * 1024;
const BLOCKS_PER_RECORD: usize = 16;
const OFFSET_RECORD_SIZE: usize = 8 + 2 * BLOCKS_PER_RECORD;
const NODE_SIZE: usize = 16;
const ROOT_NODE: u32 = 0x7FFF_FFFF;
const FILE_FLAG: u32 = 0x8000_0000;
const MAX_NAME_TABLE: u64 = 0x7FFF_FFFF;
const MAX_FILE_TREE: u64 = 0x7FFF_FFFF;
const MAX_OFFSET_RECORDS: u64 = 0xFFFF_FFFF;

struct OffsetRecord {
    offset: u64,
    sizes: [u16; BLOCKS_PER_RECORD],
}

enum Node {
    File { name_offset: u32, offset: u64, size: u64 },
    Dir { name_offset: u32, start: u32, count: u32 },
}

impl Node {
    fn name_offset(&self) -> u32 {
        match self {
            Node::File { name_offset, .. } | Node::Dir { name_offset, .. } => *name_offset,
        }
    }
}

pub struct ZArchive {
    file: File,
    data_offset: u64,
    data_len: u64,
    records: Vec<OffsetRecord>,
    names: Vec<u8>,
    nodes: Vec<Node>,
}

fn be32(d: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(d[p..p + 4].try_into().unwrap())
}
fn be64(d: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(d[p..p + 8].try_into().unwrap())
}

impl ZArchive {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("Cannot open ZArchive: {e}"))?;
        let file_len = file
            .metadata()
            .map_err(|e| format!("ZArchive: {e}"))?
            .len();
        if file_len < FOOTER_SIZE {
            return Err("Not a ZArchive (file is too small)".into());
        }

        let mut footer = [0u8; FOOTER_SIZE as usize];
        file.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))
            .map_err(|e| format!("ZArchive seek: {e}"))?;
        file.read_exact(&mut footer)
            .map_err(|e| format!("ZArchive footer: {e}"))?;

        if footer[0x8C..0x90] != MAGIC {
            return Err("Not a ZArchive (bad magic)".into());
        }
        if footer[0x88..0x8C] != VERSION_1 {
            return Err("ZArchive: only version 1 is supported".into());
        }
        if be64(&footer, 0x80) != file_len {
            return Err("ZArchive: footer size does not match the file".into());
        }

        // Each section must lie inside the file before anything is read from it.
        let section = |i: usize, limit: u64, what: &str| -> Result<(u64, u64), String> {
            let off = be64(&footer, i * 16);
            let size = be64(&footer, i * 16 + 8);
            if off.checked_add(size).is_none_or(|end| end > file_len) {
                return Err(format!("ZArchive: {what} section runs past the end of the file"));
            }
            if size > limit {
                return Err(format!("ZArchive: {what} section is implausibly large"));
            }
            Ok((off, size))
        };

        let (data_offset, data_len) = section(0, u64::MAX, "data")?;
        let (rec_off, rec_size) = section(1, MAX_OFFSET_RECORDS, "offset records")?;
        let (name_off, name_size) = section(2, MAX_NAME_TABLE, "name table")?;
        let (tree_off, tree_size) = section(3, MAX_FILE_TREE, "file tree")?;

        if rec_size % OFFSET_RECORD_SIZE as u64 != 0 {
            return Err("ZArchive: offset record section is not a whole number of records".into());
        }
        if tree_size % NODE_SIZE as u64 != 0 {
            return Err("ZArchive: file tree is not a whole number of nodes".into());
        }

        let read_section = |f: &mut File, off: u64, size: u64, what: &str| -> Result<Vec<u8>, String> {
            let mut buf = vec![0u8; size as usize];
            f.seek(SeekFrom::Start(off)).map_err(|e| format!("ZArchive {what} seek: {e}"))?;
            f.read_exact(&mut buf).map_err(|e| format!("ZArchive {what}: {e}"))?;
            Ok(buf)
        };

        let rec_raw = read_section(&mut file, rec_off, rec_size, "offset records")?;
        let records = rec_raw
            .chunks_exact(OFFSET_RECORD_SIZE)
            .map(|c| {
                let mut sizes = [0u16; BLOCKS_PER_RECORD];
                for (i, s) in sizes.iter_mut().enumerate() {
                    *s = u16::from_be_bytes(c[8 + i * 2..10 + i * 2].try_into().unwrap());
                }
                OffsetRecord { offset: be64(c, 0), sizes }
            })
            .collect();

        let names = read_section(&mut file, name_off, name_size, "name table")?;
        let tree = read_section(&mut file, tree_off, tree_size, "file tree")?;

        let nodes: Vec<Node> = tree
            .chunks_exact(NODE_SIZE)
            .map(|c| {
                let word = be32(c, 0);
                if word & FILE_FLAG != 0 {
                    let offset_low = be32(c, 4) as u64;
                    let size_low = be32(c, 8) as u64;
                    let size_high = u16::from_be_bytes(c[12..14].try_into().unwrap()) as u64;
                    let offset_high = u16::from_be_bytes(c[14..16].try_into().unwrap()) as u64;
                    Node::File {
                        name_offset: word & !FILE_FLAG,
                        offset: (offset_high << 32) | offset_low,
                        size: (size_high << 32) | size_low,
                    }
                } else {
                    Node::Dir { name_offset: word, start: be32(c, 4), count: be32(c, 8) }
                }
            })
            .collect();

        match nodes.first() {
            Some(Node::Dir { name_offset, .. }) if *name_offset & ROOT_NODE == ROOT_NODE => {}
            _ => return Err("ZArchive: first tree node is not the root directory".into()),
        }

        Ok(ZArchive { file, data_offset, data_len, records, names, nodes })
    }

    /// Names are stored length-prefixed, with a second length byte when the
    /// first has its top bit set.
    fn name_at(&self, offset: u32) -> String {
        let mut p = offset as usize;
        let Some(&first) = self.names.get(p) else { return String::new() };
        p += 1;
        let len = if first & 0x80 != 0 {
            let Some(&second) = self.names.get(p) else { return String::new() };
            p += 1;
            (first as usize & 0x7F) | ((second as usize) << 7)
        } else {
            first as usize
        };
        let end = (p + len).min(self.names.len());
        String::from_utf8_lossy(&self.names[p..end]).into_owned()
    }

    fn children(&self, index: usize) -> &[Node] {
        match self.nodes.get(index) {
            Some(Node::Dir { start, count, .. }) => {
                let s = *start as usize;
                let e = s.saturating_add(*count as usize).min(self.nodes.len());
                if s >= self.nodes.len() { &[] } else { &self.nodes[s..e] }
            }
            _ => &[],
        }
    }

    fn child_index(&self, dir_index: usize, name: &str) -> Option<usize> {
        let (start, count) = match self.nodes.get(dir_index)? {
            Node::Dir { start, count, .. } => (*start as usize, *count as usize),
            _ => return None,
        };
        (start..start.saturating_add(count).min(self.nodes.len()))
            .find(|&i| self.name_at(self.nodes[i].name_offset()).eq_ignore_ascii_case(name))
    }

    fn resolve(&self, path: &str) -> Option<usize> {
        let mut idx = 0usize;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            idx = self.child_index(idx, part)?;
        }
        Some(idx)
    }

    pub fn list_directory(&self, dir_path: &str) -> Result<Vec<DiscEntry>, String> {
        let idx = self
            .resolve(dir_path)
            .ok_or_else(|| format!("Not found: {dir_path}"))?;
        if !matches!(self.nodes.get(idx), Some(Node::Dir { .. })) {
            return Err(format!("Not a directory: {dir_path}"));
        }
        Ok(self
            .children(idx)
            .iter()
            .map(|n| {
                let name = self.name_at(n.name_offset());
                let (is_dir, size) = match n {
                    Node::File { size, .. } => (false, *size),
                    Node::Dir { .. } => (true, 0),
                };
                DiscEntry {
                    name,
                    is_dir,
                    lba: 0,
                    size: size.min(u32::MAX as u64) as u32,
                    size_bytes: size.min(u32::MAX as u64) as u32,
                    modified: String::new(),
                    deleted: false,
                    is_xa: false,
                }
            })
            .collect())
    }

    /// Read `size` bytes of file data starting at `offset` within the archive's
    /// logical data space, writing them out as they are decompressed.
    fn write_range<W: Write>(&mut self, offset: u64, size: u64, out: &mut W) -> Result<(), String> {
        let mut done = 0u64;
        let mut block_buf = vec![0u8; BLOCK_SIZE];
        while done < size {
            let absolute = offset + done;
            let block = absolute / BLOCK_SIZE as u64;
            let rec_index = (block / BLOCKS_PER_RECORD as u64) as usize;
            let within = (block % BLOCKS_PER_RECORD as u64) as usize;
            let rec = self
                .records
                .get(rec_index)
                .ok_or_else(|| format!("ZArchive: no offset record for block {block}"))?;

            // Stored sizes are one less than the real length, and blocks inside a
            // record are laid end to end from its base offset.
            let mut read_offset = rec.offset;
            for s in &rec.sizes[..within] {
                read_offset += *s as u64 + 1;
            }
            let stored = rec.sizes[within] as usize + 1;
            if read_offset + stored as u64 > self.data_len {
                return Err("ZArchive: block runs past the end of the data section".into());
            }

            let mut comp = vec![0u8; stored];
            self.file
                .seek(SeekFrom::Start(self.data_offset + read_offset))
                .map_err(|e| format!("ZArchive seek: {e}"))?;
            self.file
                .read_exact(&mut comp)
                .map_err(|e| format!("ZArchive read: {e}"))?;

            // A block that reaches the full block size was stored as-is.
            let plain: &[u8] = if stored == BLOCK_SIZE {
                &comp
            } else {
                let n = zstd::bulk::decompress_to_buffer(&comp, &mut block_buf)
                    .map_err(|e| format!("ZArchive: block {block} failed to decompress: {e}"))?;
                if n != BLOCK_SIZE {
                    return Err(format!(
                        "ZArchive: block {block} decompressed to {n} bytes, expected {BLOCK_SIZE}"
                    ));
                }
                &block_buf
            };

            let intra = (absolute % BLOCK_SIZE as u64) as usize;
            let take = ((size - done) as usize).min(BLOCK_SIZE - intra);
            out.write_all(&plain[intra..intra + take])
                .map_err(|e| format!("Write error: {e}"))?;
            done += take as u64;
        }
        Ok(())
    }

    pub fn extract_file(&mut self, file_path: &str, dest_path: &str) -> Result<(), String> {
        let idx = self
            .resolve(file_path)
            .ok_or_else(|| format!("Not found: {file_path}"))?;
        let (offset, size) = match self.nodes.get(idx) {
            Some(Node::File { offset, size, .. }) => (*offset, *size),
            _ => return Err(format!("Not a file: {file_path}")),
        };
        let mut out = File::create(dest_path).map_err(|e| format!("Cannot create file: {e}"))?;
        self.write_range(offset, size, &mut out)
    }

    pub fn extract_directory(&mut self, dir_path: &str, dest_path: &str) -> Result<(), String> {
        let idx = self
            .resolve(dir_path)
            .ok_or_else(|| format!("Not found: {dir_path}"))?;
        if !matches!(self.nodes.get(idx), Some(Node::Dir { .. })) {
            return Err(format!("Not a directory: {dir_path}"));
        }
        std::fs::create_dir_all(dest_path).map_err(|e| format!("Cannot create directory: {e}"))?;

        // Walk by index rather than recursing, so a deep tree cannot blow the stack.
        let mut stack = vec![(idx, dest_path.to_string(), dir_path.trim_end_matches('/').to_string())];
        while let Some((node, dest, src)) = stack.pop() {
            let (start, count) = match self.nodes[node] {
                Node::Dir { start, count, .. } => (start as usize, count as usize),
                _ => continue,
            };
            for i in start..start.saturating_add(count).min(self.nodes.len()) {
                let name = self.name_at(self.nodes[i].name_offset());
                let safe = crate::sanitize_component(&name);
                let child_dest = format!("{dest}/{safe}");
                let child_src = format!("{src}/{name}");
                match self.nodes[i] {
                    Node::Dir { .. } => {
                        std::fs::create_dir_all(&child_dest)
                            .map_err(|e| format!("Cannot create directory: {e}"))?;
                        stack.push((i, child_dest, child_src));
                    }
                    Node::File { offset, size, .. } => {
                        let mut out = File::create(&child_dest)
                            .map_err(|e| format!("Cannot create file: {e}"))?;
                        self.write_range(offset, size, &mut out)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Cheap signature check, so a mis-named file is rejected before the full parse.
pub fn is_zarchive(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    if f.seek(SeekFrom::End(-4)).is_err() {
        return false;
    }
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).is_ok() && magic == MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Builder {
        names: Vec<u8>,
        nodes: Vec<[u8; NODE_SIZE]>,
        // Stored bytes per *global* block. Offset records cover 16 consecutive
        // blocks of the whole archive, not of one file, so the record table can
        // only be built once every file has been added.
        blocks: Vec<Vec<u8>>,
    }

    impl Builder {
        fn new() -> Self {
            Builder { names: Vec::new(), nodes: Vec::new(), blocks: Vec::new() }
        }

        fn add_name(&mut self, name: &str) -> u32 {
            let off = self.names.len() as u32;
            assert!(name.len() < 0x80, "test names stay in the short form");
            self.names.push(name.len() as u8);
            self.names.extend_from_slice(name.as_bytes());
            off
        }

        // Append a file's bytes as whole 64 KB blocks, alternating zstd-compressed
        // and stored verbatim so both paths are exercised.
        fn add_file_data(&mut self, content: &[u8]) -> u64 {
            let offset = (self.blocks.len() * BLOCK_SIZE) as u64;
            let count = content.len().div_ceil(BLOCK_SIZE).max(1);
            for b in 0..count {
                let mut chunk =
                    content[b * BLOCK_SIZE..((b + 1) * BLOCK_SIZE).min(content.len())].to_vec();
                chunk.resize(BLOCK_SIZE, 0);
                let stored = if self.blocks.len() % 2 == 0 {
                    zstd::bulk::compress(&chunk, 3).unwrap()
                } else {
                    chunk
                };
                assert!(stored.len() <= BLOCK_SIZE);
                self.blocks.push(stored);
            }
            offset
        }

        fn dir_node(&mut self, name_off: u32, start: u32, count: u32) {
            let mut n = [0u8; NODE_SIZE];
            n[0..4].copy_from_slice(&name_off.to_be_bytes());
            n[4..8].copy_from_slice(&start.to_be_bytes());
            n[8..12].copy_from_slice(&count.to_be_bytes());
            self.nodes.push(n);
        }

        fn file_node(&mut self, name_off: u32, offset: u64, size: u64) {
            let mut n = [0u8; NODE_SIZE];
            n[0..4].copy_from_slice(&(name_off | FILE_FLAG).to_be_bytes());
            n[4..8].copy_from_slice(&(offset as u32).to_be_bytes());
            n[8..12].copy_from_slice(&(size as u32).to_be_bytes());
            n[12..14].copy_from_slice(&((size >> 32) as u16).to_be_bytes());
            n[14..16].copy_from_slice(&((offset >> 32) as u16).to_be_bytes());
            self.nodes.push(n);
        }

        fn finish(self, path: &Path) {
            // Data first, then a record per group of 16 blocks: base offset of the
            // group followed by each block's stored length minus one.
            let mut data = Vec::new();
            let mut records = Vec::new();
            for group in self.blocks.chunks(BLOCKS_PER_RECORD) {
                records.extend_from_slice(&(data.len() as u64).to_be_bytes());
                let mut sizes = [0u16; BLOCKS_PER_RECORD];
                for (i, blk) in group.iter().enumerate() {
                    sizes[i] = (blk.len() - 1) as u16;
                    data.extend_from_slice(blk);
                }
                for s in sizes {
                    records.extend_from_slice(&s.to_be_bytes());
                }
            }

            let mut out = Vec::new();
            let data_off = 0u64;
            out.extend_from_slice(&data);
            let rec_off = out.len() as u64;
            out.extend_from_slice(&records);
            let name_off = out.len() as u64;
            out.extend_from_slice(&self.names);
            let tree_off = out.len() as u64;
            for n in &self.nodes {
                out.extend_from_slice(n);
            }
            let meta_off = out.len() as u64;

            let mut footer = Vec::new();
            for (o, s) in [
                (data_off, data.len() as u64),
                (rec_off, records.len() as u64),
                (name_off, self.names.len() as u64),
                (tree_off, (self.nodes.len() * NODE_SIZE) as u64),
                (meta_off, 0),
                (meta_off, 0),
            ] {
                footer.extend_from_slice(&o.to_be_bytes());
                footer.extend_from_slice(&s.to_be_bytes());
            }
            footer.extend_from_slice(&[0u8; 32]); // integrity hash, unverified
            let total = out.len() as u64 + FOOTER_SIZE;
            footer.extend_from_slice(&total.to_be_bytes());
            footer.extend_from_slice(&VERSION_1);
            footer.extend_from_slice(&MAGIC);
            assert_eq!(footer.len() as u64, FOOTER_SIZE);
            out.extend_from_slice(&footer);
            std::fs::write(path, out).unwrap();
        }
    }

    // root
    //   hello.txt
    //   content/
    //     big.bin
    fn build(dir: &Path) -> (std::path::PathBuf, Vec<u8>, Vec<u8>) {
        let mut b = Builder::new();
        let hello = b"Hello from inside a Wii U Archive.".to_vec();
        let big: Vec<u8> = (0..BLOCK_SIZE * 2 + 1234).map(|i| (i * 31 % 251) as u8).collect();

        let n_root = ROOT_NODE;
        let n_hello = b.add_name("hello.txt");
        let n_content = b.add_name("content");
        let n_big = b.add_name("big.bin");

        let off_hello = b.add_file_data(&hello);
        let off_big = b.add_file_data(&big);

        b.dir_node(n_root, 1, 2); // 0: root -> [1,2]
        b.file_node(n_hello, off_hello, hello.len() as u64); // 1
        b.dir_node(n_content, 3, 1); // 2: content -> [3]
        b.file_node(n_big, off_big, big.len() as u64); // 3

        let p = dir.join("title.wua");
        b.finish(&p);
        (p, hello, big)
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dx_wua_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lists_the_tree() {
        let d = scratch("list");
        let (p, _, big) = build(&d);
        assert!(is_zarchive(&p));

        let z = ZArchive::open(&p).unwrap();
        let root = z.list_directory("/").unwrap();
        let names: Vec<_> = root.iter().map(|e| (e.name.as_str(), e.is_dir)).collect();
        assert_eq!(names, vec![("hello.txt", false), ("content", true)]);

        let sub = z.list_directory("/content").unwrap();
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].name, "big.bin");
        assert_eq!(sub[0].size_bytes as usize, big.len());

        assert!(z.list_directory("/nope").is_err());
        assert!(z.list_directory("/hello.txt").is_err(), "a file is not a directory");
        let _ = std::fs::remove_dir_all(&d);
    }

    // The interesting case: a file spanning several blocks, where some were
    // stored verbatim and some compressed, and whose length is not a block
    // multiple — so the last block must be truncated to the file size.
    #[test]
    fn extracts_files_byte_for_byte() {
        let d = scratch("extract");
        let (p, hello, big) = build(&d);
        let mut z = ZArchive::open(&p).unwrap();

        let out = d.join("hello.out");
        z.extract_file("/hello.txt", out.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), hello);

        let out2 = d.join("big.out");
        z.extract_file("/content/big.bin", out2.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&out2).unwrap(), big);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn extracts_a_whole_tree() {
        let d = scratch("tree");
        let (p, hello, big) = build(&d);
        let mut z = ZArchive::open(&p).unwrap();

        let dest = d.join("out");
        z.extract_directory("/", dest.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(dest.join("hello.txt")).unwrap(), hello);
        assert_eq!(std::fs::read(dest.join("content/big.bin")).unwrap(), big);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_files_that_are_not_zarchives() {
        let d = scratch("reject");
        let p = d.join("nope.wua");
        std::fs::write(&p, vec![0u8; 200]).unwrap();
        assert!(!is_zarchive(&p));
        assert!(ZArchive::open(&p).is_err());

        // Right magic, wrong declared size — the footer must agree with the file.
        let (good, _, _) = build(&d);
        let mut bytes = std::fs::read(&good).unwrap();
        let n = bytes.len();
        bytes[n - 16..n - 8].copy_from_slice(&999u64.to_be_bytes());
        let bad = d.join("bad.wua");
        std::fs::write(&bad, bytes).unwrap();
        let err = ZArchive::open(&bad).err().expect("should reject");
        assert!(err.contains("does not match"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
