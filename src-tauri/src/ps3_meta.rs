// PlayStation 3 disc metadata: PARAM.SFO and PS3_DISC.SFB.
//
// A PS3 disc leaves its ISO 9660 volume identifier blank, so a PS3 image shows
// no name at all in the sidebar. The disc does carry its title, but in these two
// files instead:
//
//   PS3_DISC.SFB   — in the disc root, fixed-layout, big-endian. Holds the title
//                    ID and disc version.
//   PS3_GAME/PARAM.SFO — a little-endian key/value table holding TITLE, the
//                    human-readable name, along with APP_VER and others.
//
// SFO layout (little-endian except the magic):
//   [0x00] u32 BE  magic = "\0PSF"
//   [0x04] u32     version
//   [0x08] u32     key table start
//   [0x0C] u32     data table start
//   [0x10] u32     number of entries
//   then one 16-byte index entry per key:
//     u16 key offset (into the key table), u16 data format,
//     u32 data length, u32 data max length, u32 data offset
//
// Layout derived from SabreTools.Serialization (MIT, Copyright (c) 2018-2026
// Matt Nadareski), SFO.cs and SFB.cs.

use std::collections::BTreeMap;

const SFO_MAGIC: &[u8; 4] = b"\0PSF";
const SFB_MAGIC: &[u8; 4] = b".SFB";
const INDEX_ENTRY_SIZE: usize = 16;

const FORMAT_UTF8_SPECIAL: u16 = 0x0004;
const FORMAT_UTF8: u16 = 0x0204;
const FORMAT_INTEGER: u16 = 0x0404;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfoValue {
    Text(String),
    Integer(u32),
}

impl SfoValue {
    pub fn as_text(&self) -> String {
        match self {
            SfoValue::Text(s) => s.clone(),
            SfoValue::Integer(n) => n.to_string(),
        }
    }
}

fn le16(d: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(p..p + 2)?.try_into().ok()?))
}
fn le32(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(p..p + 4)?.try_into().ok()?))
}
fn be32(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(p..p + 4)?.try_into().ok()?))
}

/// Strings in both formats are NUL-padded to their field width.
fn trim_nul(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

/// Parse PARAM.SFO into its key/value pairs. Entries whose offsets fall outside
/// the file are skipped rather than failing the whole parse — a truncated SFO
/// still usually yields the title.
pub fn parse_sfo(data: &[u8]) -> Result<BTreeMap<String, SfoValue>, String> {
    if data.len() < 20 || &data[0..4] != SFO_MAGIC {
        return Err("Not a PARAM.SFO file".into());
    }
    let key_start = le32(data, 8).ok_or("SFO: truncated header")? as usize;
    let data_start = le32(data, 12).ok_or("SFO: truncated header")? as usize;
    let count = le32(data, 16).ok_or("SFO: truncated header")? as usize;

    // Each entry costs 16 bytes; a count that cannot fit is a corrupt header.
    if 20 + count.saturating_mul(INDEX_ENTRY_SIZE) > data.len() {
        return Err(format!("SFO: entry count {count} does not fit the file"));
    }

    let mut out = BTreeMap::new();
    for i in 0..count {
        let p = 20 + i * INDEX_ENTRY_SIZE;
        let (Some(key_off), Some(format), Some(len), Some(val_off)) = (
            le16(data, p),
            le16(data, p + 2),
            le32(data, p + 4),
            le32(data, p + 12),
        ) else {
            continue;
        };

        let key_at = key_start + key_off as usize;
        let Some(key_bytes) = data.get(key_at..) else { continue };
        let key = trim_nul(key_bytes);
        if key.is_empty() {
            continue;
        }

        let val_at = data_start + val_off as usize;
        let Some(raw) = data.get(val_at..val_at + len as usize) else { continue };
        let value = match format {
            FORMAT_INTEGER => SfoValue::Integer(le32(raw, 0).unwrap_or(0)),
            FORMAT_UTF8 | FORMAT_UTF8_SPECIAL => SfoValue::Text(trim_nul(raw)),
            _ => continue,
        };
        out.insert(key, value);
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sfb {
    pub title_id: String,
    pub version: String,
}

/// Parse PS3_DISC.SFB. Everything is at a fixed offset, and the fields are
/// NUL-padded to a fixed width.
pub fn parse_sfb(data: &[u8]) -> Result<Sfb, String> {
    if data.len() < 0x240 || &data[0..4] != SFB_MAGIC {
        return Err("Not a PS3_DISC.SFB file".into());
    }
    // 0x20 bytes of disc content at 0x200, then a 0x10 title field and a 0x10
    // version field.
    let title_id = trim_nul(&data[0x220..0x230]);
    let version = trim_nul(&data[0x230..0x240]);
    Ok(Sfb { title_id, version })
}

/// The disc's own name, assembled from whichever of the two files is present.
/// Prefers PARAM.SFO's TITLE, since that is the name a player would recognise,
/// and falls back to the SFB's title ID.
pub fn disc_title(sfo: Option<&BTreeMap<String, SfoValue>>, sfb: Option<&Sfb>) -> String {
    if let Some(t) = sfo.and_then(|m| m.get("TITLE")) {
        let t = t.as_text();
        if !t.is_empty() {
            return t;
        }
    }
    sfb.map(|s| s.title_id.clone()).unwrap_or_default()
}

/// `_be32` is kept for callers reading the SFB's big-endian length fields.
pub fn sfb_content_length(data: &[u8]) -> Option<u32> {
    be32(data, 0x38)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_sfo(entries: &[(&str, SfoValue)]) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut values = Vec::new();
        let mut index = Vec::new();

        for (k, v) in entries {
            let key_off = keys.len() as u16;
            keys.extend_from_slice(k.as_bytes());
            keys.push(0);

            let val_off = values.len() as u32;
            let (format, bytes) = match v {
                SfoValue::Text(s) => {
                    let mut b = s.as_bytes().to_vec();
                    b.push(0);
                    (FORMAT_UTF8, b)
                }
                SfoValue::Integer(n) => (FORMAT_INTEGER, n.to_le_bytes().to_vec()),
            };
            let len = bytes.len() as u32;
            values.extend_from_slice(&bytes);

            index.extend_from_slice(&key_off.to_le_bytes());
            index.extend_from_slice(&format.to_le_bytes());
            index.extend_from_slice(&len.to_le_bytes());
            index.extend_from_slice(&len.to_le_bytes());
            index.extend_from_slice(&val_off.to_le_bytes());
        }

        let key_start = 20 + index.len() as u32;
        let data_start = key_start + keys.len() as u32;

        let mut out = Vec::new();
        out.extend_from_slice(SFO_MAGIC);
        out.extend_from_slice(&0x0101_0000u32.to_le_bytes());
        out.extend_from_slice(&key_start.to_le_bytes());
        out.extend_from_slice(&data_start.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        out.extend_from_slice(&index);
        out.extend_from_slice(&keys);
        out.extend_from_slice(&values);
        out
    }

    #[test]
    fn reads_title_and_version_out_of_an_sfo() {
        let sfo = build_sfo(&[
            ("APP_VER", SfoValue::Text("01.00".into())),
            ("PARENTAL_LEVEL", SfoValue::Integer(5)),
            ("TITLE", SfoValue::Text("Demon's Souls".into())),
            ("TITLE_ID", SfoValue::Text("BLES00932".into())),
        ]);
        let m = parse_sfo(&sfo).unwrap();
        assert_eq!(m.get("TITLE").unwrap().as_text(), "Demon's Souls");
        assert_eq!(m.get("TITLE_ID").unwrap().as_text(), "BLES00932");
        assert_eq!(m.get("PARENTAL_LEVEL"), Some(&SfoValue::Integer(5)));
        assert_eq!(disc_title(Some(&m), None), "Demon's Souls");
    }

    #[test]
    fn rejects_what_is_not_an_sfo() {
        assert!(parse_sfo(b"nope").is_err());
        let mut bad = build_sfo(&[("TITLE", SfoValue::Text("X".into()))]);
        bad[16..20].copy_from_slice(&9999u32.to_le_bytes());
        assert!(parse_sfo(&bad).unwrap_err().contains("does not fit"));
    }

    // A truncated table should still surrender the entries that are intact.
    #[test]
    fn survives_entries_pointing_outside_the_file() {
        let sfo = build_sfo(&[
            ("TITLE", SfoValue::Text("Good".into())),
            ("OTHER", SfoValue::Text("Also good".into())),
        ]);
        let cut = &sfo[..sfo.len() - 5];
        let m = parse_sfo(cut).unwrap();
        assert_eq!(m.get("TITLE").unwrap().as_text(), "Good");
        assert!(!m.contains_key("OTHER"), "the entry that ran off the end is dropped");
    }

    #[test]
    fn reads_an_sfb() {
        let mut d = vec![0u8; 0x240];
        d[0..4].copy_from_slice(SFB_MAGIC);
        d[0x220..0x229].copy_from_slice(b"BLES00932");
        d[0x230..0x235].copy_from_slice(b"01.00");
        let sfb = parse_sfb(&d).unwrap();
        assert_eq!(sfb.title_id, "BLES00932");
        assert_eq!(sfb.version, "01.00");
        assert_eq!(disc_title(None, Some(&sfb)), "BLES00932");

        d[0] = b'X';
        assert!(parse_sfb(&d).is_err());
        assert!(parse_sfb(&d[..0x100]).is_err(), "too short to hold the fields");
    }
}
