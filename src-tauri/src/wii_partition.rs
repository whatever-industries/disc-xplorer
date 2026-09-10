// Wii disc partition decryption.
//
// Wii game data lives inside an AES-128-CBC encrypted data partition.
// This module transparently decrypts clusters so that GcmFs can read
// the game's FST and file data normally.
//
// Cluster layout (0x8000 bytes encrypted → 0x7C00 bytes of game data):
//   [0x0000:0x0400] Hash block  (not decrypted; raw bytes [0x3D0:0x3E0] are the data IV)
//   [0x0400:0x8000] Data block  — AES-CBC decrypt, IV = raw[0x3D0:0x3E0]
//
// Key derivation:
//   1. Encrypted title key at ticket+0x01BF (16 bytes)
//   2. IV = title ID (8 bytes at ticket+0x01DC) + 8 zero bytes
//   3. Decrypt with the common key the ticket names at +0x01F1
//
// Every disc carries its own title key, which is why a Wii disc needs no key
// file from the user — unlike PS3 or Wii U. What it does need is the right
// common key to unwrap that title key with, and there are three.
//
// Sources: libwbfs (Wiimm, GPL-2.0), libogc, Dolphin Emulator (GPL-2.0)

use std::io::{self, Read, Seek, SeekFrom};
use aes::Aes128;
use cbc::Decryptor;
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};

// The three publicly known Wii common keys, indexed as the ticket indexes them.
// All are embedded in every Wii emulator. Korean discs and vWii titles use their
// own, and unwrapping a Korean title key with the retail key yields a plausible
// but wrong key, so the partition decrypts to noise rather than failing.
const COMMON_KEYS: [[u8; 16]; 3] = [
    // 0 — retail
    [0xEB, 0xE4, 0x2A, 0x22, 0x5E, 0x85, 0x93, 0xE4,
     0x48, 0xD9, 0xC5, 0x45, 0x73, 0x81, 0xAA, 0xF7],
    // 1 — Korean
    [0x63, 0xB8, 0x2B, 0xB4, 0xF4, 0x61, 0x4E, 0x2E,
     0x13, 0xF2, 0xFE, 0xFB, 0xBA, 0x4C, 0x9B, 0x7E],
    // 2 — vWii
    [0x30, 0xBF, 0xC7, 0x6E, 0x7C, 0x19, 0xAF, 0xBB,
     0x23, 0x16, 0x33, 0x30, 0xCE, 0xD7, 0xC2, 0x8D],
];

const CLUSTER_ENC:  u64   = 0x8000; // encrypted bytes per cluster
const CLUSTER_DATA: u64   = 0x7C00; // decrypted data bytes per cluster
const HASH_SIZE:    usize = 0x400;  // hash block at cluster start
const DATA_IV_OFF:  usize = 0x3D0;  // IV offset within encrypted hash block

pub struct WiiPartReader<F: Read + Seek> {
    inner:      F,
    data_start: u64,        // absolute disc offset to first encrypted cluster
    virt_size:  u64,        // total bytes in decrypted stream
    title_key:  [u8; 16],
    pos:        u64,
    cache_idx:  Option<u64>,
    cache_buf:  Vec<u8>,    // CLUSTER_DATA decrypted bytes
}

impl<F: Read + Seek> WiiPartReader<F> {
    pub fn open(mut inner: F) -> Result<Self, String> {
        let part_off = find_data_partition(&mut inner)
            .ok_or_else(|| "Wii: no data partition found".to_string())?;

        // Ticket: encrypted title key at +0x1BF, title ID at +0x1DC
        inner.seek(SeekFrom::Start(part_off + 0x1BF))
            .map_err(|e| format!("Wii ticket seek: {e}"))?;
        let mut enc_key = [0u8; 16];
        inner.read_exact(&mut enc_key)
            .map_err(|e| format!("Wii title key read: {e}"))?;

        inner.seek(SeekFrom::Start(part_off + 0x1DC))
            .map_err(|e| format!("Wii title ID seek: {e}"))?;
        let mut key_iv = [0u8; 16]; // last 8 bytes stay zero
        inner.read_exact(&mut key_iv[..8])
            .map_err(|e| format!("Wii title ID read: {e}"))?;

        // Which common key unwraps this ticket's title key.
        inner.seek(SeekFrom::Start(part_off + 0x1F1))
            .map_err(|e| format!("Wii key index seek: {e}"))?;
        let mut idx = [0u8; 1];
        inner.read_exact(&mut idx)
            .map_err(|e| format!("Wii key index read: {e}"))?;
        let common_key = COMMON_KEYS
            .get(idx[0] as usize)
            .copied()
            .unwrap_or(COMMON_KEYS[0]);

        let mut title_key = enc_key;
        type AesDec = Decryptor<Aes128>;
        AesDec::new(&common_key.into(), &key_iv.into())
            .decrypt_padded_mut::<NoPadding>(&mut title_key)
            .map_err(|_| "Wii: title key decrypt failed".to_string())?;

        // Partition header at +0x2B8: data_offset (u32 ×4 relative to partition start), data_size (u32 ×4)
        inner.seek(SeekFrom::Start(part_off + 0x2B8))
            .map_err(|e| format!("Wii part header seek: {e}"))?;
        let mut buf = [0u8; 8];
        inner.read_exact(&mut buf)
            .map_err(|e| format!("Wii part header read: {e}"))?;
        let data_off_raw  = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64;
        let data_size_raw = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as u64;
        let data_start = part_off + (data_off_raw << 2);
        let data_size  = data_size_raw << 2;
        let virt_size  = (data_size / CLUSTER_ENC) * CLUSTER_DATA;

        Ok(WiiPartReader {
            inner,
            data_start,
            virt_size,
            title_key,
            pos: 0,
            cache_idx: None,
            cache_buf: vec![0u8; CLUSTER_DATA as usize],
        })
    }

    fn decrypt_cluster(&mut self, idx: u64) -> io::Result<()> {
        self.inner.seek(SeekFrom::Start(self.data_start + idx * CLUSTER_ENC))?;
        let mut raw = vec![0u8; CLUSTER_ENC as usize];
        self.inner.read_exact(&mut raw)?;

        type AesDec = Decryptor<Aes128>;

        // IV for data block comes from the *encrypted* hash block at 0x3D0 (not decrypted).
        let mut data_iv = [0u8; 16];
        data_iv.copy_from_slice(&raw[DATA_IV_OFF..DATA_IV_OFF + 16]);

        // Decrypt data block
        let mut data = raw[HASH_SIZE..].to_vec();
        AesDec::new(&self.title_key.into(), &data_iv.into())
            .decrypt_padded_mut::<NoPadding>(&mut data)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "data decrypt failed"))?;

        self.cache_buf.copy_from_slice(&data);
        self.cache_idx = Some(idx);
        Ok(())
    }
}

impl<F: Read + Seek> Read for WiiPartReader<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.virt_size { return Ok(0); }
        let cluster_idx = self.pos / CLUSTER_DATA;
        let cluster_off = (self.pos % CLUSTER_DATA) as usize;
        if self.cache_idx != Some(cluster_idx) {
            self.decrypt_cluster(cluster_idx)?;
        }
        let avail = (CLUSTER_DATA as usize - cluster_off)
            .min((self.virt_size - self.pos) as usize);
        let n = buf.len().min(avail);
        buf[..n].copy_from_slice(&self.cache_buf[cluster_off..cluster_off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<F: Read + Seek> Seek for WiiPartReader<F> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.pos = match pos {
            SeekFrom::Start(n)   => n,
            SeekFrom::End(n)     => if n >= 0 { self.virt_size.saturating_add(n as u64) }
                                    else       { self.virt_size.saturating_sub((-n) as u64) },
            SeekFrom::Current(n) => if n >= 0 { self.pos.saturating_add(n as u64) }
                                    else       { self.pos.saturating_sub((-n) as u64) },
        };
        Ok(self.pos)
    }
}

/// Partition groups in the table at 0x40000. The format allows four.
const MAX_GROUPS: usize = 4;
/// A generous cap on partitions within one group. A real disc carries one or
/// two, the game and an update; the format has no use for hundreds.
const MAX_PARTS_PER_GROUP: usize = 16;
/// Every Wii ticket opens with this signature type, RSA-2048 with SHA-1.
const TICKET_SIG_TYPE: u32 = 0x0001_0001;

/// Does a Wii ticket really begin at `at`?
///
/// This is what separates a partition table from bytes that merely parse like
/// one. A ticket starts with its signature type and, at 0x140, an issuer that
/// always begins "Root-CA". Arbitrary file data does not satisfy both.
fn looks_like_ticket<F: Read + Seek>(reader: &mut F, at: u64, len: u64) -> bool {
    if at.saturating_add(0x2C0) > len {
        return false;
    }
    let mut sig = [0u8; 4];
    if reader.seek(SeekFrom::Start(at)).is_err() || reader.read_exact(&mut sig).is_err() {
        return false;
    }
    if u32::from_be_bytes(sig) != TICKET_SIG_TYPE {
        return false;
    }
    let mut issuer = [0u8; 7];
    reader.seek(SeekFrom::Start(at + 0x140)).is_ok()
        && reader.read_exact(&mut issuer).is_ok()
        && &issuer == b"Root-CA"
}

/// Locate the game data partition through the table at 0x40000.
///
/// Nothing in that table identifies itself, so every value read from it has to
/// be checked against the file before it is trusted. Without those checks, a
/// disc that is not a Wii disc at all parses as a partition table: ordinary file
/// data at 0x40000 gives a count in the billions and an offset somewhere in the
/// image, and the first four zero bytes found from there read as a data
/// partition. That is how a 37 GB PS4 BD-ROM came to be labelled "Wii GCM",
/// since this runs as a fallback for discs whose header magic is missing.
fn find_data_partition<F: Read + Seek>(reader: &mut F) -> Option<u64> {
    let len = reader.seek(SeekFrom::End(0)).ok()?;
    reader.seek(SeekFrom::Start(0x40000)).ok()?;
    let mut hdr = [0u8; 32]; // 4 groups x 8 bytes each
    reader.read_exact(&mut hdr).ok()?;

    for g in 0..MAX_GROUPS {
        let count = u32::from_be_bytes([hdr[g * 8], hdr[g * 8 + 1], hdr[g * 8 + 2], hdr[g * 8 + 3]]) as usize;
        let tbl_off = (u32::from_be_bytes([hdr[g * 8 + 4], hdr[g * 8 + 5], hdr[g * 8 + 6], hdr[g * 8 + 7]]) as u64) << 2;
        if count == 0 || count > MAX_PARTS_PER_GROUP {
            continue;
        }
        let table_bytes = (count * 8) as u64;
        if tbl_off.saturating_add(table_bytes) > len {
            continue;
        }
        // Read the whole table first: checking each entry seeks elsewhere.
        let mut table = vec![0u8; count * 8];
        if reader.seek(SeekFrom::Start(tbl_off)).is_err() || reader.read_exact(&mut table).is_err() {
            continue;
        }
        for e in table.chunks_exact(8) {
            let part_off = (u32::from_be_bytes([e[0], e[1], e[2], e[3]]) as u64) << 2;
            let part_type = u32::from_be_bytes([e[4], e[5], e[6], e[7]]);
            // Type 0 is the game data partition, and a ticket must be sitting
            // there, or this was never a partition table.
            if part_type == 0 && looks_like_ticket(reader, part_off, len) {
                return Some(part_off);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The exact bytes found at 0x40000 in a 37 GB PS4 BD-ROM that this reader
    /// once accepted as a Wii disc. Group 0 reads as a count of 1.78 billion
    /// partitions at an offset 805 MB into the file; from there the first four
    /// zero bytes anywhere looked like a data partition.
    const PS4_BDROM_AT_0X40000: [u8; 32] = [
        0x6a, 0x00, 0x8e, 0x85, 0x0c, 0x00, 0x00, 0x0c,
        0x85, 0x8e, 0xa9, 0x3b, 0x12, 0x00, 0x00, 0x12,
        0x3b, 0xa9, 0x73, 0x0c, 0x13, 0x0f, 0x0a, 0x22,
        0xe0, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01,
    ];

    /// An image with `hdr` at 0x40000 and zeroes elsewhere, which is the shape
    /// that used to be misread: zeroes are what a type-0 partition looks like.
    fn image_with_header(hdr: &[u8; 32], size: usize) -> Cursor<Vec<u8>> {
        let mut v = vec![0u8; size];
        v[0x40000..0x40000 + 32].copy_from_slice(hdr);
        Cursor::new(v)
    }

    #[test]
    fn garbage_at_the_partition_table_is_not_a_wii_disc() {
        let mut img = image_with_header(&PS4_BDROM_AT_0X40000, 0x8_0000);
        assert_eq!(find_data_partition(&mut img), None);
    }

    /// The counts are plausible here, so only the ticket check rejects it.
    #[test]
    fn a_plausible_table_without_a_ticket_is_rejected() {
        let mut hdr = [0u8; 32];
        hdr[0..4].copy_from_slice(&2u32.to_be_bytes()); // count
        hdr[4..8].copy_from_slice(&(0x40020u32 >> 2).to_be_bytes()); // table offset
        let mut img = image_with_header(&hdr, 0x8_0000);
        // Table entries are zero, so part_off 0 with type 0: a data partition
        // by the old rules, but there is no ticket at offset 0.
        assert_eq!(find_data_partition(&mut img), None);
    }

    /// The same table, but with a real ticket where it points. This is the
    /// layout a genuine Wii disc has, and it must still be found.
    #[test]
    fn a_table_pointing_at_a_real_ticket_is_accepted() {
        let part_off = 0x50000u64;
        let mut v = vec![0u8; 0x8_0000];
        v[0x40000..0x40004].copy_from_slice(&1u32.to_be_bytes()); // one partition
        v[0x40004..0x40008].copy_from_slice(&((0x40020u32) >> 2).to_be_bytes());
        v[0x40020..0x40024].copy_from_slice(&((part_off as u32) >> 2).to_be_bytes());
        v[0x40024..0x40028].copy_from_slice(&0u32.to_be_bytes()); // type 0: data
        let at = part_off as usize;
        v[at..at + 4].copy_from_slice(&TICKET_SIG_TYPE.to_be_bytes());
        v[at + 0x140..at + 0x147].copy_from_slice(b"Root-CA");
        assert_eq!(find_data_partition(&mut Cursor::new(v)), Some(part_off));
    }

    /// A count large enough to scan for a long time is refused outright rather
    /// than being walked; that scan is what made the bug slow as well as wrong.
    #[test]
    fn an_absurd_partition_count_is_refused() {
        let mut hdr = [0u8; 32];
        hdr[0..4].copy_from_slice(&1_000_000u32.to_be_bytes());
        hdr[4..8].copy_from_slice(&(0x40020u32 >> 2).to_be_bytes());
        let mut img = image_with_header(&hdr, 0x8_0000);
        assert_eq!(find_data_partition(&mut img), None);
    }
}
