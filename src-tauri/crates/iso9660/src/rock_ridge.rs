// SPDX-License-Identifier: (MIT OR Apache-2.0)
//
// Minimal Rock Ridge (IEEE P1282) / SUSP (IEEE P1281) support.
//
// Rock Ridge stores POSIX metadata in the "System Use" area at the tail of each
// ISO 9660 directory record. We only consume what is needed to present a faithful
// file tree: the `SP` indicator (presence + skip length), `NM` alternate names
// (POSIX long names, including continuations), and `CE` continuation areas that
// let those fields overflow into separate logical blocks.

use crate::fileref::{FileRef, ISO9660Reader};

// Guard against pathological / circular CE chains.
const MAX_AREAS: usize = 64;

fn read_le32(d: &[u8]) -> u32 {
    u32::from_le_bytes([d[0], d[1], d[2], d[3]])
}

// Read a System Use continuation area referenced by a `CE` entry: a byte range
// (`offset`, `length`) within the logical block at `block`.
fn read_continuation<T: ISO9660Reader>(
    file: &FileRef<T>,
    block: u32,
    offset: u32,
    length: u32,
) -> Option<Vec<u8>> {
    if length == 0 {
        return None;
    }
    let mut buf = [0u8; 2048];
    let n = file.read_at(&mut buf, block as u64).ok()?;
    let n = n.min(2048);
    let start = (offset as usize).min(n);
    let end = (start + length as usize).min(n);
    if end <= start {
        return None;
    }
    Some(buf[start..end].to_vec())
}

/// If `root_dot_su` (the System Use area of a directory's "." record) carries a
/// SUSP `SP` indicator, return the number of bytes to skip at the start of every
/// System Use area on this volume (`LEN_SKP`). `None` means no SUSP/Rock Ridge.
pub(crate) fn susp_skip(root_dot_su: &[u8]) -> Option<usize> {
    let mut p = 0;
    while p + 4 <= root_dot_su.len() {
        let len = root_dot_su[p + 2] as usize;
        if len < 4 || p + len > root_dot_su.len() {
            break;
        }
        if &root_dot_su[p..p + 2] == b"SP"
            && len >= 7
            && root_dot_su[p + 4] == 0xBE
            && root_dot_su[p + 5] == 0xEF
        {
            return Some(root_dot_su[p + 6] as usize);
        }
        p += len;
    }
    None
}

/// Resolve the Rock Ridge alternate name (`NM`) for a directory record, following
/// `CE` continuation areas. Returns `None` when there is no usable `NM` (e.g. the
/// "." / ".." records, or records that only carry other SUSP fields).
pub(crate) fn alternate_name<T: ISO9660Reader>(
    system_use: &[u8],
    skip: usize,
    file: &FileRef<T>,
) -> Option<String> {
    let mut name = String::new();
    let mut found = false;
    let mut is_dot_entry = false;

    // Process areas in order so multi-part NM (CONTINUE flag) concatenates
    // correctly across a CE boundary. New continuations are appended at the back.
    let mut areas: Vec<Vec<u8>> = vec![system_use.to_vec()];
    let mut idx = 0;
    while idx < areas.len() && idx < MAX_AREAS {
        let area = areas[idx].clone();
        idx += 1;

        let mut p = skip.min(area.len());
        while p + 4 <= area.len() {
            let len = area[p + 2] as usize;
            if len < 4 || p + len > area.len() {
                break;
            }
            let sig = [area[p], area[p + 1]];
            let data = &area[p + 4..p + len];
            match &sig {
                b"NM" => {
                    if let Some((&flags, rest)) = data.split_first() {
                        // Bits 1 (CURRENT, ".") and 2 (PARENT, "..") mark the
                        // relocated self/parent names, which we leave as "."/"..".
                        if flags & 0b0000_0110 != 0 {
                            is_dot_entry = true;
                        } else {
                            name.push_str(&String::from_utf8_lossy(rest));
                            found = true;
                        }
                    }
                }
                b"CE" if data.len() >= 24 => {
                    let block = read_le32(&data[0..4]);
                    let offset = read_le32(&data[8..12]);
                    let length = read_le32(&data[16..20]);
                    if areas.len() < MAX_AREAS {
                        if let Some(cont) = read_continuation(file, block, offset, length) {
                            areas.push(cont);
                        }
                    }
                }
                b"ST" => break,
                _ => {}
            }
            p += len;
        }
    }

    if found && !is_dot_entry {
        Some(name)
    } else {
        None
    }
}
