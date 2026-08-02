// IRD (PS3 disc info) parsing.
//
// An IRD file records everything needed to verify and decrypt one PS3 disc:
// the title, the region and per-file MD5 hashes, the Blu-ray PIC, and — the
// part that matters here — the D1 (data1) key. IRD is how PS3 disc keys are
// actually catalogued and distributed, so an IRD sitting next to an image is
// a more likely find than a bare .dkey.
//
// The file is gzip-compressed; the structure below is the decompressed form.
// Field order shifts between versions: v7 carries the UID early, v9 moved the
// PIC ahead of the keys, and v8+ put the UID at the end.
//
// Layout derived from SabreTools.Serialization (MIT, Copyright (c) 2018-2026
// Matt Nadareski), IRD.cs.

use std::io::Read;
use std::path::Path;

pub const MAGIC: &[u8; 4] = b"3IRD";

#[derive(Debug, Clone)]
pub struct Ird {
    pub version: u8,
    pub title_id: String,
    pub title: String,
    pub system_version: String,
    pub game_version: String,
    pub app_version: String,
    pub region_count: u8,
    pub file_count: u32,
    pub data1_key: [u8; 16],
    pub data2_key: [u8; 16],
    pub pic: Vec<u8>,
    pub uid: u32,
}

struct Cursor<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.p.checked_add(n).ok_or("IRD: length overflow")?;
        if end > self.d.len() {
            return Err(format!(
                "IRD: truncated at offset {} (wanted {n} bytes, {} left)",
                self.p,
                self.d.len().saturating_sub(self.p)
            ));
        }
        let s = &self.d[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn key(&mut self) -> Result<[u8; 16], String> {
        Ok(self.take(16)?.try_into().unwrap())
    }
}

/// True when the file looks like an IRD, gzipped or not. Cheap enough to run
/// before committing to a full parse.
pub fn is_ird(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; 4];
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    &head == MAGIC || head[..2] == [0x1F, 0x8B]
}

fn decompress(raw: Vec<u8>) -> Result<Vec<u8>, String> {
    if raw.len() >= 2 && raw[..2] == [0x1F, 0x8B] {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&raw[..])
            .read_to_end(&mut out)
            .map_err(|e| format!("IRD: cannot decompress: {e}"))?;
        Ok(out)
    } else {
        Ok(raw)
    }
}

pub fn parse_file(path: &Path) -> Result<Ird, String> {
    let raw = std::fs::read(path).map_err(|e| format!("Cannot read IRD: {e}"))?;
    parse(&decompress(raw)?)
}

pub fn parse(data: &[u8]) -> Result<Ird, String> {
    let mut c = Cursor { d: data, p: 0 };

    if c.take(4)? != MAGIC {
        return Err("Not an IRD file (missing 3IRD signature)".into());
    }
    let version = c.u8()?;
    if version < 6 {
        return Err(format!("IRD version {version} is not supported (need 6 or later)"));
    }

    let title_id = String::from_utf8_lossy(c.take(9)?).trim().to_string();
    let title_len = c.u8()? as usize;
    let title = String::from_utf8_lossy(c.take(title_len)?).trim().to_string();
    let system_version = String::from_utf8_lossy(c.take(4)?).trim().to_string();
    let game_version = String::from_utf8_lossy(c.take(5)?).trim().to_string();
    let app_version = String::from_utf8_lossy(c.take(5)?).trim().to_string();

    // v7 alone carries the UID here rather than at the end.
    let mut uid = if version == 7 { c.u32()? } else { 0 };

    let header_len = c.u32()? as usize;
    c.take(header_len)?;
    let footer_len = c.u32()? as usize;
    c.take(footer_len)?;

    let region_count = c.u8()?;
    c.take(region_count as usize * 16)?;

    let file_count = c.u32()?;
    // Each entry is an 8-byte sector offset followed by a 16-byte MD5.
    let files_len = (file_count as usize)
        .checked_mul(24)
        .ok_or("IRD: implausible file count")?;
    c.take(files_len)?;

    let _extra_config = c.u16()?;
    let _attachments = c.u16()?;

    // v9 moved the PIC in front of the keys.
    let mut pic = Vec::new();
    if version >= 9 {
        pic = c.take(115)?.to_vec();
    }
    let data1_key = c.key()?;
    let data2_key = c.key()?;
    if version < 9 {
        pic = c.take(115)?.to_vec();
    }
    if version > 7 {
        uid = c.u32()?;
    }

    Ok(Ird {
        version,
        title_id,
        title,
        system_version,
        game_version,
        app_version,
        region_count,
        file_count,
        data1_key,
        data2_key,
        pic,
        uid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Build a synthetic v9 IRD: the field order is the whole point of the parser,
    // so a round trip through it is what needs proving.
    fn synth(version: u8) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(MAGIC);
        d.push(version);
        d.extend_from_slice(b"BLES00001");
        d.push(9);
        d.extend_from_slice(b"Test Game");
        d.extend_from_slice(b"4.75");
        d.extend_from_slice(b"01.00");
        d.extend_from_slice(b"01.01");
        if version == 7 {
            d.extend_from_slice(&7u32.to_le_bytes());
        }
        d.extend_from_slice(&3u32.to_le_bytes());
        d.extend_from_slice(&[0xAA; 3]); // header blob
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&[0xBB; 2]); // footer blob
        d.push(2); // region count
        d.extend_from_slice(&[0xCC; 32]);
        d.extend_from_slice(&2u32.to_le_bytes()); // file count
        d.extend_from_slice(&[0xDD; 48]);
        d.extend_from_slice(&0u16.to_le_bytes()); // extra config
        d.extend_from_slice(&0u16.to_le_bytes()); // attachments
        if version >= 9 {
            d.extend_from_slice(&[0xEE; 115]);
        }
        d.extend_from_slice(&[0x11; 16]); // data1
        d.extend_from_slice(&[0x22; 16]); // data2
        if version < 9 {
            d.extend_from_slice(&[0xEE; 115]);
        }
        if version > 7 {
            d.extend_from_slice(&42u32.to_le_bytes());
        }
        d.extend_from_slice(&0u32.to_le_bytes()); // crc
        d
    }

    #[test]
    fn parses_every_supported_version_layout() {
        for v in [6u8, 7, 8, 9] {
            let ird = parse(&synth(v)).unwrap_or_else(|e| panic!("v{v}: {e}"));
            assert_eq!(ird.version, v);
            assert_eq!(ird.title_id, "BLES00001");
            assert_eq!(ird.title, "Test Game");
            assert_eq!(ird.data1_key, [0x11; 16], "v{v} data1 key");
            assert_eq!(ird.data2_key, [0x22; 16], "v{v} data2 key");
            assert_eq!(ird.pic.len(), 115, "v{v} PIC");
            assert_eq!(ird.pic[0], 0xEE);
            assert_eq!(ird.region_count, 2);
            assert_eq!(ird.file_count, 2);
        }
        // The UID sits in a different place either side of v8.
        assert_eq!(parse(&synth(7)).unwrap().uid, 7);
        assert_eq!(parse(&synth(9)).unwrap().uid, 42);
    }

    #[test]
    fn rejects_what_is_not_an_ird() {
        assert!(parse(b"not an ird at all").is_err());
        let mut old = synth(9);
        old[4] = 5; // version 5
        assert!(parse(&old).unwrap_err().contains("not supported"));
    }

    #[test]
    fn truncation_is_an_error_not_a_panic() {
        let full = synth(9);
        for cut in [10, 40, full.len() - 20] {
            assert!(parse(&full[..cut]).is_err(), "cut at {cut} should fail");
        }
    }

    #[test]
    fn reads_a_gzipped_ird_from_disk() {
        let dir = std::env::temp_dir().join("dx_ird_test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("game.ird");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&synth(9)).unwrap();
        std::fs::write(&p, enc.finish().unwrap()).unwrap();

        assert!(is_ird(&p));
        let ird = parse_file(&p).unwrap();
        assert_eq!(ird.data1_key, [0x11; 16]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
