// SPDX-License-Identifier: (MIT OR Apache-2.0)

use nom::branch::alt;
use nom::bytes::complete::{tag, take};
use nom::combinator::map;
use nom::number::complete::*;
use nom::sequence::tuple;
use nom::IResult;
use time::OffsetDateTime;

use super::both_endian::{both_endian16, both_endian32};
use super::date_time::{date_time_ascii, date_time_ascii_hsg};
use super::directory_entry::{directory_entry, DirectoryEntryHeader};
use crate::ISOError;

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum VolumeDescriptor {
    Primary {
        system_identifier: String,
        volume_identifier: String,
        volume_space_size: u32,
        volume_set_size: u16,
        volume_sequence_number: u16,
        logical_block_size: u16,

        path_table_size: u32,
        path_table_loc: u32,
        optional_path_table_loc: u32,

        root_directory_entry: DirectoryEntryHeader,
        root_directory_entry_identifier: String,

        volume_set_identifier: String,
        publisher_identifier: String,
        data_preparer_identifier: String,
        application_identifier: String,
        copyright_file_identifier: String,
        abstract_file_identifier: String,
        bibliographic_file_identifier: String,

        creation_time: OffsetDateTime,
        modification_time: OffsetDateTime,
        expiration_time: OffsetDateTime,
        effective_time: OffsetDateTime,

        file_structure_version: u8,

        // True for a High Sierra (pre-ISO 9660) volume, whose directory records
        // use the 6-byte date / offset-24 flags layout.
        high_sierra: bool,
    },
    Supplementary {
        // True when the escape sequences identify a Joliet (UCS-2) name space.
        joliet: bool,
        logical_block_size: u16,
        root_directory_entry: DirectoryEntryHeader,
        root_directory_entry_identifier: String,
    },
    BootRecord {
        boot_system_identifier: String,
        boot_identifier: String,
        data: Vec<u8>,
    },
    VolumeDescriptorSetTerminator,
}

impl VolumeDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Option<VolumeDescriptor>, ISOError> {
        Ok(volume_descriptor(bytes)?.1)
    }
}

fn take_string_trim(count: usize) -> impl Fn(&[u8]) -> IResult<&[u8], String> {
    // These descriptor fields are nominally ASCII, but real discs (e.g. Apple
    // hybrids with a "© Apple" application identifier) carry stray high bytes.
    // Decode leniently so one bad byte in an informational field can't abort the
    // whole volume-descriptor parse and make the disc unreadable.
    move |i: &[u8]| {
        map(take(count), |b: &[u8]| {
            String::from_utf8_lossy(b).trim_end().to_string()
        })(i)
    }
}

fn boot_record(i: &[u8]) -> IResult<&[u8], VolumeDescriptor> {
    let (i, (boot_system_identifier, boot_identifier, data)) = tuple((
        take_string_trim(32usize),
        take_string_trim(32usize),
        take(1977usize),
    ))(i)?;
    Ok((
        i,
        VolumeDescriptor::BootRecord {
            boot_system_identifier,
            boot_identifier,
            data: data.to_vec(),
        },
    ))
}

fn volume_descriptor(i: &[u8]) -> IResult<&[u8], Option<VolumeDescriptor>> {
    // High Sierra volume descriptors begin with an 8-byte descriptor-LBN field,
    // so the type code sits at offset 8 and the "CDROM" standard identifier at
    // offset 9 (vs. ISO 9660's type at 0 and "CD001" at 1).
    if i.len() >= 14 && &i[9..14] == b"CDROM" {
        return high_sierra_descriptor(i);
    }
    let (i, type_code) = le_u8(i)?;
    let (i, _) = alt((tag("CD001\u{1}"), tag("CD-I \u{1}")))(i)?;
    match type_code {
        0 => map(boot_record, Some)(i),
        1 => map(|i| primary_descriptor(i, false), Some)(i),
        2 => map(supplementary_descriptor, Some)(i),
        //3 => map!(volume_partition_descriptor, Some)(i),
        255 => Ok((i, Some(VolumeDescriptor::VolumeDescriptorSetTerminator))),
        _ => Ok((i, None)),
    }
}

fn high_sierra_descriptor(i: &[u8]) -> IResult<&[u8], Option<VolumeDescriptor>> {
    let (i, _lbn) = take(8usize)(i)?; // descriptor logical sector number
    let (i, type_code) = le_u8(i)?;
    let (i, _) = tag("CDROM")(i)?;
    let (i, _version) = le_u8(i)?;
    match type_code {
        1 => map(|i| primary_descriptor(i, true), Some)(i),
        255 => Ok((i, Some(VolumeDescriptor::VolumeDescriptorSetTerminator))),
        _ => Ok((i, None)),
    }
}

fn primary_descriptor(i: &[u8], high_sierra: bool) -> IResult<&[u8], VolumeDescriptor> {
    let (i, _) = take(1usize)(i)?; // padding / reserved
    let (i, system_identifier) = take_string_trim(32usize)(i)?;
    let (i, volume_identifier) = take_string_trim(32usize)(i)?;
    let (i, _) = take(8usize)(i)?; // padding
    let (i, volume_space_size) = both_endian32(i)?;
    let (i, _) = take(32usize)(i)?; // padding
    let (i, volume_set_size) = both_endian16(i)?;
    let (i, volume_sequence_number) = both_endian16(i)?;
    let (i, logical_block_size) = both_endian16(i)?;
    let (i, path_table_size) = both_endian32(i)?;

    // High Sierra stores four Type-L then four Type-M path table pointers (a
    // primary plus three optionals each); ISO 9660 stores two of each.
    let (i, path_table_loc) = le_u32(i)?;
    let (i, optional_path_table_loc) = le_u32(i)?;
    let i = if high_sierra {
        let (i, _) = take(4usize)(i)?; // optional Type-L 2
        let (i, _) = take(4usize)(i)?; // optional Type-L 3
        let (i, _) = take(4usize)(i)?; // Type-M path table
        let (i, _) = take(4usize)(i)?; // optional Type-M 1
        let (i, _) = take(4usize)(i)?; // optional Type-M 2
        let (i, _) = take(4usize)(i)?; // optional Type-M 3
        i
    } else {
        let (i, _) = take(4usize)(i)?; // path_table_loc_be
        let (i, _) = take(4usize)(i)?; // optional_path_table_loc_be
        i
    };

    let (i, root_directory_entry) = directory_entry(i, false, high_sierra)?;

    let (i, volume_set_identifier) = take_string_trim(128)(i)?;
    let (i, publisher_identifier) = take_string_trim(128)(i)?;
    let (i, data_preparer_identifier) = take_string_trim(128)(i)?;
    let (i, application_identifier) = take_string_trim(128)(i)?;

    // High Sierra: 32/32-byte copyright/abstract identifiers and no
    // bibliographic field. ISO 9660: 38/36/37.
    let (i, copyright_file_identifier, abstract_file_identifier, bibliographic_file_identifier) =
        if high_sierra {
            let (i, c) = take_string_trim(32)(i)?;
            let (i, a) = take_string_trim(32)(i)?;
            (i, c, a, String::new())
        } else {
            let (i, c) = take_string_trim(38)(i)?;
            let (i, a) = take_string_trim(36)(i)?;
            let (i, b) = take_string_trim(37)(i)?;
            (i, c, a, b)
        };

    // High Sierra volume dates are 16 bytes; ISO 9660's are 17.
    let date = |i| if high_sierra { date_time_ascii_hsg(i) } else { date_time_ascii(i) };
    let (i, creation_time) = date(i)?;
    let (i, modification_time) = date(i)?;
    let (i, expiration_time) = date(i)?;
    let (i, effective_time) = date(i)?;

    let (i, file_structure_version) = le_u8(i)?;

    Ok((
        i,
        VolumeDescriptor::Primary {
            system_identifier,
            volume_identifier,
            volume_space_size,
            volume_set_size,
            volume_sequence_number,
            logical_block_size,

            path_table_size,
            path_table_loc,
            optional_path_table_loc,

            root_directory_entry: root_directory_entry.0,
            root_directory_entry_identifier: root_directory_entry.1,

            volume_set_identifier,
            publisher_identifier,
            data_preparer_identifier,
            application_identifier,
            copyright_file_identifier,
            abstract_file_identifier,
            bibliographic_file_identifier,

            creation_time,
            modification_time,
            expiration_time,
            effective_time,

            file_structure_version,

            high_sierra,
        },
    ))
}

// A supplementary volume descriptor (type 2) is byte-for-byte identical in
// layout to the primary descriptor, except that the 32-byte reserved field at
// offset 88 carries escape sequences. When those select UCS-2 (Joliet), the
// directory tree it points at uses big-endian UTF-16 file identifiers.
fn supplementary_descriptor(i: &[u8]) -> IResult<&[u8], VolumeDescriptor> {
    let (i, _flags) = take(1usize)(i)?; // volume flags
    let (i, _system_identifier) = take(32usize)(i)?;
    let (i, _volume_identifier) = take(32usize)(i)?;
    let (i, _) = take(8usize)(i)?; // padding
    let (i, _volume_space_size) = both_endian32(i)?;
    let (i, escape_sequences) = take(32usize)(i)?;
    let (i, _volume_set_size) = both_endian16(i)?;
    let (i, _volume_sequence_number) = both_endian16(i)?;
    let (i, logical_block_size) = both_endian16(i)?;
    let (i, _path_table_size) = both_endian32(i)?;
    let (i, _path_table_loc) = le_u32(i)?;
    let (i, _optional_path_table_loc) = le_u32(i)?;
    let (i, _) = take(4usize)(i)?; // path_table_loc_be
    let (i, _) = take(4usize)(i)?; // optional_path_table_loc_be

    // Root directory record identifier is a single 0x00 byte; decode as standard.
    let (i, root_directory_entry) = directory_entry(i, false, false)?;

    let joliet = escape_sequences.starts_with(b"%/@")
        || escape_sequences.starts_with(b"%/C")
        || escape_sequences.starts_with(b"%/E");

    Ok((
        i,
        VolumeDescriptor::Supplementary {
            joliet,
            logical_block_size,
            root_directory_entry: root_directory_entry.0,
            root_directory_entry_identifier: root_directory_entry.1,
        },
    ))
}
