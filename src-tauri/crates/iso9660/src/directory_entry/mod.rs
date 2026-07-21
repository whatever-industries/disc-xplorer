// SPDX-License-Identifier: (MIT OR Apache-2.0)

pub use self::isodirectory::{ISODirectory, ISODirectoryIterator};
pub use self::isofile::{ISOFile, ISOFileReader};

use crate::parse::{DirectoryEntryHeader, FileFlags};
use crate::{FileRef, ISO9660Reader, Result};

mod isodirectory;
mod isofile;

#[derive(Clone, Debug)]
pub enum DirectoryEntry<T: ISO9660Reader> {
    Directory(ISODirectory<T>),
    File(ISOFile<T>),
}

impl<T: ISO9660Reader> DirectoryEntry<T> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        header: DirectoryEntryHeader,
        identifier: String,
        file: FileRef<T>,
        joliet: bool,
        high_sierra: bool,
        rock_ridge: bool,
        susp_skip: usize,
    ) -> Result<Self> {
        if header.file_flags.contains(FileFlags::DIRECTORY) {
            Ok(DirectoryEntry::Directory(ISODirectory::new(
                header, identifier, file, joliet, high_sierra, rock_ridge, susp_skip,
            )))
        } else {
            Ok(DirectoryEntry::File(ISOFile::new(
                header, identifier, file,
            )?))
        }
    }

    pub fn header(&self) -> &DirectoryEntryHeader {
        match *self {
            DirectoryEntry::Directory(ref dir) => &dir.header,
            DirectoryEntry::File(ref file) => &file.header,
        }
    }

    pub fn identifier(&self) -> &str {
        match *self {
            DirectoryEntry::Directory(ref dir) => &dir.identifier,
            DirectoryEntry::File(ref file) => &file.identifier,
        }
    }
}
