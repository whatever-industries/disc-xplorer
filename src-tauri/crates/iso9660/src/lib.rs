// SPDX-License-Identifier: (MIT OR Apache-2.0)

#![cfg_attr(feature = "nightly", feature(read_initializer, specialization))]

extern crate time;
#[macro_use]
extern crate bitflags;
extern crate nom;

use std::result;

pub use directory_entry::{
    DirectoryEntry, ISODirectory, ISODirectoryIterator, ISOFile, ISOFileReader,
};
pub use error::ISOError;
pub(crate) use fileref::FileRef;
pub use fileref::ISO9660Reader;
use parse::{DirectoryEntryHeader, VolumeDescriptor};

pub type Result<T> = result::Result<T, ISOError>;

mod directory_entry;
mod error;
mod fileref;
mod parse;
mod rock_ridge;

/// Which name space of an ISO 9660 volume to traverse. A single physical disc can
/// expose the same files under several name spaces simultaneously.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NameSpace {
    /// Primary volume descriptor (8.3-style ISO 9660 / Joliet-less names).
    Iso,
    /// Joliet supplementary descriptor (UCS-2 long names).
    Joliet,
    /// Primary tree with Rock Ridge (`NM`) POSIX names applied.
    RockRidge,
}

pub struct ISO9660<T: ISO9660Reader> {
    _file: FileRef<T>,
    pub root: ISODirectory<T>,
    // Root of the Joliet (UCS-2) name space, when the disc carries a Joliet
    // supplementary volume descriptor.
    pub joliet_root: Option<ISODirectory<T>>,
    // Primary root configured to resolve Rock Ridge names, when SUSP/Rock Ridge
    // is present on the volume.
    rr_root: Option<ISODirectory<T>>,
    primary: VolumeDescriptor,
}

macro_rules! primary_prop_str {
    ($name:ident) => {
        pub fn $name(&self) -> &str {
            if let VolumeDescriptor::Primary { $name, .. } = &self.primary {
                &$name
            } else {
                unreachable!()
            }
        }
    };
}

impl<T: ISO9660Reader> ISO9660<T> {
    pub fn new(mut reader: T) -> Result<ISO9660<T>> {
        let mut buf: [u8; 2048] = [0; 2048];
        let mut root = None;
        let mut joliet = None;
        let mut primary = None;

        // Skip the "system area"
        let mut lba = 16;

        // Read volume descriptors
        loop {
            let count = reader.read_at(&mut buf, lba)?;

            if count != 2048 {
                return Err(ISOError::ReadSize(2048, count));
            }

            let descriptor = VolumeDescriptor::parse(&buf)?;
            match &descriptor {
                Some(VolumeDescriptor::Primary {
                    logical_block_size,
                    root_directory_entry,
                    root_directory_entry_identifier,
                    ..
                }) => {
                    // CD-i (Green Book) stores block size as LE=0, BE=2048;
                    // accept 0 as equivalent to 2048.
                    if *logical_block_size != 2048 && *logical_block_size != 0 {
                        return Err(ISOError::InvalidFs("Block size not 2048"));
                    }

                    root = Some((
                        root_directory_entry.clone(),
                        root_directory_entry_identifier.clone(),
                    ));
                    primary = descriptor;
                }
                Some(VolumeDescriptor::Supplementary {
                    joliet: true,
                    logical_block_size,
                    root_directory_entry,
                    root_directory_entry_identifier,
                }) => {
                    if *logical_block_size == 2048 || *logical_block_size == 0 {
                        joliet = Some((
                            root_directory_entry.clone(),
                            root_directory_entry_identifier.clone(),
                        ));
                    }
                }
                Some(VolumeDescriptor::VolumeDescriptorSetTerminator) => break,
                _ => {}
            }

            lba += 1;
        }

        let file = FileRef::new(reader);

        let (root, primary) = match (root, primary) {
            (Some(root), Some(primary)) => (root, primary),
            _ => {
                return Err(ISOError::InvalidFs("No primary volume descriptor"));
            }
        };

        let joliet_root =
            joliet.map(|j| ISODirectory::new(j.0, j.1, file.clone(), true, false, 0));

        // Detect Rock Ridge / SUSP by inspecting the System Use area of the
        // primary root's "." record. When present, expose a second view of the
        // primary tree that resolves POSIX `NM` names.
        let rr_root = read_root_dot_system_use(&file, &root.0)
            .as_deref()
            .and_then(rock_ridge::susp_skip)
            .map(|skip| ISODirectory::new(root.0.clone(), root.1.clone(), file.clone(), false, true, skip));

        Ok(ISO9660 {
            root: ISODirectory::new(root.0, root.1, file.clone(), false, false, 0),
            joliet_root,
            rr_root,
            _file: file,
            primary,
        })
    }

    pub fn open(&self, path: &str) -> Result<Option<DirectoryEntry<T>>> {
        self.open_view(path, NameSpace::Iso)
    }

    /// Resolve `path` against the requested name space. Falls back to the primary
    /// tree when the requested name space is not present on the volume.
    pub fn open_view(&self, path: &str, ns: NameSpace) -> Result<Option<DirectoryEntry<T>>> {
        let start = self.root_dir(ns);
        // TODO: avoid clone()
        let mut entry = DirectoryEntry::Directory(start.clone());
        for segment in path.split('/').filter(|x| !x.is_empty()) {
            let parent = match entry {
                DirectoryEntry::Directory(dir) => dir,
                _ => return Ok(None),
            };

            entry = match parent.find(segment)? {
                Some(entry) => entry,
                None => return Ok(None),
            };
        }

        Ok(Some(entry))
    }

    /// The directory to start traversal from for the requested name space.
    /// Falls back to the primary tree when the requested name space is absent.
    pub fn root_dir(&self, ns: NameSpace) -> &ISODirectory<T> {
        match ns {
            NameSpace::Iso => &self.root,
            NameSpace::Joliet => self.joliet_root.as_ref().unwrap_or(&self.root),
            NameSpace::RockRidge => self.rr_root.as_ref().unwrap_or(&self.root),
        }
    }

    /// Whether the disc carries a Joliet supplementary volume descriptor.
    pub fn has_joliet(&self) -> bool {
        self.joliet_root.is_some()
    }

    /// Whether the disc carries Rock Ridge / SUSP extensions.
    pub fn has_rock_ridge(&self) -> bool {
        self.rr_root.is_some()
    }

    pub fn block_size(&self) -> u16 {
        2048 // XXX
    }

    primary_prop_str!(volume_set_identifier);
    primary_prop_str!(publisher_identifier);
    primary_prop_str!(data_preparer_identifier);
    primary_prop_str!(application_identifier);
    primary_prop_str!(copyright_file_identifier);
    primary_prop_str!(abstract_file_identifier);
    primary_prop_str!(bibliographic_file_identifier);
}

// Read the System Use area of a directory's "." record (the first record in the
// directory's extent). Used to probe for the SUSP `SP` indicator on the root.
fn read_root_dot_system_use<T: ISO9660Reader>(
    file: &FileRef<T>,
    root: &DirectoryEntryHeader,
) -> Option<Vec<u8>> {
    let mut buf = [0u8; 2048];
    if file.read_at(&mut buf, root.extent_loc as u64).ok()? != 2048 {
        return None;
    }
    DirectoryEntryHeader::parse(&buf, false).ok().map(|(_, _, su)| su)
}
