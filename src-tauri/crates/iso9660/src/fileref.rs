// SPDX-License-Identifier: (MIT OR Apache-2.0)

use std::cell::RefCell;
#[cfg(feature = "nightly")]
use std::fs::File;
use std::io::{Read, Result, Seek, SeekFrom};
use std::rc::Rc;

pub trait ISO9660Reader {
    /// Read the block(s) at a given LBA (logical block address)
    fn read_at(&mut self, buf: &mut [u8], lba: u64) -> Result<usize>;

    /// Read the raw CD-ROM XA payload (subheader + data + EDC, i.e. the 2336
    /// bytes following sync+header) of the sector at `lba`. Only meaningful for
    /// Mode 2 raw-sector sources; the default returns 0 to signal "unsupported",
    /// so callers fall back to the logical 2048-byte view. Used to extract Mode 2
    /// Form 2 files (PSX .XA / .STR) without truncating to 2048 bytes/sector.
    fn read_raw_sector(&mut self, _lba: u64, _out: &mut [u8]) -> Result<usize> {
        Ok(0)
    }
}

#[cfg(not(feature = "nightly"))]
impl<T: Read + Seek> ISO9660Reader for T {
    fn read_at(&mut self, buf: &mut [u8], lba: u64) -> Result<usize> {
        self.seek(SeekFrom::Start(lba * 2048))?;
        self.read(buf)
    }
}

#[cfg(feature = "nightly")]
impl<T: Read + Seek> ISO9660Reader for T {
    default fn read_at(&mut self, buf: &mut [u8], lba: u64) -> Result<usize> {
        self.seek(SeekFrom::Start(lba * 2048))?;
        self.read(buf)?
    }
}

#[cfg(feature = "nightly")]
impl ISO9660Reader for File {
    fn read_at(&mut self, buf: &mut [u8], lba: u64) -> Result<usize> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            FileExt::read_at(self, buf, lba * 2048)?
        }
        #[cfg(not(unix))]
        {
            use std::io::{Read, Seek, SeekFrom};
            self.seek(SeekFrom::Start(lba * 2048))?;
            self.read(buf)?
        }
    }
}

// TODO: Figure out if sane API possible without Rc/RefCell
pub(crate) struct FileRef<T: ISO9660Reader>(Rc<RefCell<T>>);

impl<T: ISO9660Reader> Clone for FileRef<T> {
    fn clone(&self) -> FileRef<T> {
        FileRef(self.0.clone())
    }
}

impl<T: ISO9660Reader> FileRef<T> {
    pub fn new(reader: T) -> FileRef<T> {
        FileRef(Rc::new(RefCell::new(reader)))
    }

    /// Read the block(s) at a given LBA (logical block address)
    pub fn read_at(&self, buf: &mut [u8], lba: u64) -> Result<usize> {
        (*self.0).borrow_mut().read_at(buf, lba)
    }

    /// Raw XA sector read; see [`ISO9660Reader::read_raw_sector`].
    pub fn read_raw_sector(&self, lba: u64, out: &mut [u8]) -> Result<usize> {
        (*self.0).borrow_mut().read_raw_sector(lba, out)
    }
}
