// SPDX-License-Identifier: (MIT OR Apache-2.0)

use time::OffsetDateTime;

use super::both_endian::{both_endian16, both_endian32};
use super::date_time::{date_time, date_time_hsg};
use crate::Result;
use nom::bytes::complete::take;
use nom::multi::length_data;
use nom::number::complete::le_u8;
use nom::IResult;
use std::cmp::min;

bitflags! {
    #[derive(Clone, Debug)]
    pub struct FileFlags: u8 {
        const EXISTANCE = 1 << 0;
        const DIRECTORY = 1 << 1;
        const ASSOCIATEDFILE = 1 << 2;
        const RECORD = 1 << 3;
        const PROTECTION = 1 << 4;
        // Bits 5 and 6 are reserved; should be zero
        const MULTIEXTENT = 1 << 7;
    }
}

#[derive(Clone, Debug)]
pub struct DirectoryEntryHeader {
    pub length: u8,
    pub extended_attribute_record_length: u8,
    pub extent_loc: u32,
    pub extent_length: u32,
    pub time: OffsetDateTime,
    pub file_flags: FileFlags,
    pub file_unit_size: u8,
    pub interleave_gap_size: u8,
    pub volume_sequence_number: u16,
}

impl DirectoryEntryHeader {
    pub fn parse(
        input: &[u8],
        joliet: bool,
        high_sierra: bool,
    ) -> Result<(DirectoryEntryHeader, String, Vec<u8>)> {
        Ok(directory_entry(input, joliet, high_sierra)?.1)
    }
}

// Decode a raw file identifier. Standard ISO 9660 identifiers are interpreted as
// UTF-8 (a lossy superset of the d-characters actually permitted), while Joliet
// identifiers are UCS-2 (UTF-16) big-endian. The special "." and ".." entries
// are encoded as single 0x00 / 0x01 bytes in both forms.
fn decode_identifier(raw: &[u8], joliet: bool) -> String {
    if raw.len() == 1 && (raw[0] == 0 || raw[0] == 1) {
        return (raw[0] as char).to_string();
    }
    if joliet {
        raw.chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .map(|u| char::from_u32(u as u32).unwrap_or('\u{FFFD}'))
            .collect()
    } else {
        String::from_utf8_lossy(raw).into_owned()
    }
}

pub fn directory_entry(
    i: &[u8],
    joliet: bool,
    high_sierra: bool,
) -> IResult<&[u8], (DirectoryEntryHeader, String, Vec<u8>)> {
    let (i, length) = le_u8(i)?;
    let (i, extended_attribute_record_length) = le_u8(i)?;
    let (i, extent_loc) = both_endian32(i)?;
    let (i, extent_length) = both_endian32(i)?;
    // The two formats agree up to here (offset 18) and reconverge at the file
    // identifier length (offset 32), but differ in between: High Sierra uses a
    // 6-byte date + flags + reserved + 2 interleave bytes, whereas ISO 9660 uses
    // a 7-byte date + flags + file-unit + interleave-gap.
    let (i, time, file_flags, file_unit_size, interleave_gap_size, volume_sequence_number) =
        if high_sierra {
            let (i, time) = date_time_hsg(i)?;
            let (i, file_flags) = le_u8(i)?;
            let (i, _reserved) = le_u8(i)?;
            let (i, _interleave_size) = le_u8(i)?;
            let (i, interleave_skip) = le_u8(i)?;
            let (i, volume_sequence_number) = both_endian16(i)?;
            (i, time, file_flags, 0u8, interleave_skip, volume_sequence_number)
        } else {
            let (i, time) = date_time(i)?;
            let (i, file_flags) = le_u8(i)?;
            let (i, file_unit_size) = le_u8(i)?;
            let (i, interleave_gap_size) = le_u8(i)?;
            let (i, volume_sequence_number) = both_endian16(i)?;
            (i, time, file_flags, file_unit_size, interleave_gap_size, volume_sequence_number)
        };
    let len_fi = i.first().copied().unwrap_or(0) as usize;
    let (i, raw_identifier) = length_data(le_u8)(i)?;
    let identifier = decode_identifier(raw_identifier, joliet);

    // After the file identifier comes an optional padding byte (present when the
    // identifier length is even) followed by the System Use area, which extends
    // to the end of the record (LEN_DR). Rock Ridge / SUSP entries live there.
    let pad = if len_fi % 2 == 0 { 1 } else { 0 };
    let consumed = 33 + len_fi; // fixed header (33) + identifier
    let su_len = (length as usize)
        .saturating_sub(consumed + pad)
        .min(i.len().saturating_sub(pad.min(i.len())));
    let (i, _) = take(min(pad, i.len()))(i)?;
    let (i, su) = take(su_len)(i)?;
    let system_use = su.to_vec();

    Ok((
        i,
        (
            DirectoryEntryHeader {
                length,
                extended_attribute_record_length,
                extent_loc,
                extent_length,
                time,
                file_flags: FileFlags::from_bits_truncate(file_flags),
                file_unit_size,
                interleave_gap_size,
                volume_sequence_number,
            },
            identifier,
            system_use,
        ),
    ))
}
