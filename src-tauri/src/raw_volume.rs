//! Reading an optical volume that the host cannot mount.
//!
//! macOS gets this for free: when a Wii or GameCube disc is inserted it fails to
//! mount, our drive list falls back to the `/dev/diskN` node, and that node
//! serves ordinary unaligned reads. Windows has no equivalent. The drive letter
//! is all we get, and opening `D:\` on a disc Windows does not recognise fails
//! with ERROR_UNRECOGNIZED_VOLUME (os error 1005) — which is exactly what a user
//! saw on a Wii disc in issue #14.
//!
//! The sectors are readable, through the raw volume path `\\.\D:`. The catch is
//! that reads through that handle must be whole sectors at sector-aligned
//! offsets, while every filesystem reader here seeks and reads arbitrarily: six
//! bytes at some odd offset is normal. `AlignedReader` sits between the two,
//! turning arbitrary reads into aligned ones.
//!
//! It is generic rather than Windows-only so the arithmetic can be tested on any
//! platform; only `open` is Windows-specific.

use std::io::{self, Read, Seek, SeekFrom};

/// Bytes pulled from the device per underlying read. A multiple of the 2048-byte
/// sector, large enough that walking a directory does not cause a read per file.
pub const WINDOW: u64 = 64 * 1024;

pub struct AlignedReader<F: Read + Seek> {
    inner: F,
    /// Where the caller thinks it is, in bytes, unrestricted by alignment.
    pos: u64,
    len: u64,
    /// The last window read, as (aligned offset, bytes). Sequential reads inside
    /// one window are served from here without touching the device.
    cache: Option<(u64, Vec<u8>)>,
}

// `open` below is the only caller of these outside the tests, and it is
// Windows-only, so a macOS or Linux build sees them as unused. That is the
// usual trap with platform-gated code: "never used" here does not mean unused.
#[allow(dead_code)]
impl<F: Read + Seek> AlignedReader<F> {
    pub fn new(inner: F, len: u64) -> Self {
        AlignedReader { inner, pos: 0, len, cache: None }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// Make sure the window covering `pos` is in the cache.
    fn load(&mut self, pos: u64) -> io::Result<()> {
        let start = (pos / WINDOW) * WINDOW;
        if self.cache.as_ref().is_some_and(|(at, _)| *at == start) {
            return Ok(());
        }
        // The tail of the volume is short of a full window. Volume lengths are a
        // whole number of sectors, so the clamped length stays sector-aligned.
        let want = WINDOW.min(self.len.saturating_sub(start)) as usize;
        if want == 0 {
            self.cache = Some((start, Vec::new()));
            return Ok(());
        }
        self.inner.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; want];
        self.inner.read_exact(&mut buf)?;
        self.cache = Some((start, buf));
        Ok(())
    }
}

impl<F: Read + Seek> Read for AlignedReader<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        self.load(self.pos)?;
        let (start, window) = self.cache.as_ref().expect("just loaded");
        let within = (self.pos - start) as usize;
        if within >= window.len() {
            return Ok(0);
        }
        // Serve only to the end of this window; a caller wanting more will come
        // back round, which is what Read permits and read_exact relies on.
        let n = buf.len().min(window.len() - within);
        buf[..n].copy_from_slice(&window[within..within + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<F: Read + Seek> Seek for AlignedReader<F> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(n) => self.pos as i128 + n as i128,
            SeekFrom::End(n) => self.len as i128 + n as i128,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        // Seeking past the end is allowed, as it is for a file; reads there
        // simply return nothing.
        self.pos = target as u64;
        Ok(self.pos)
    }
}

/// Open a Windows volume by drive letter through its raw device path.
///
/// `drive` is a bare letter and colon, such as `D:`. Access is read-only and
/// shared, so this does not disturb anything else reading the disc.
#[cfg(target_os = "windows")]
pub fn open(drive: &str) -> Result<AlignedReader<std::fs::File>, String> {
    use std::fs::OpenOptions;
    let device = format!(r"\\.\{}", drive.trim_end_matches(['\\', '/']));
    let mut file = OpenOptions::new()
        .read(true)
        .open(&device)
        .map_err(|e| match e.kind() {
            // The likeliest failure, and one the user can act on themselves, so
            // it says what to do rather than quoting Windows at them.
            io::ErrorKind::PermissionDenied => format!(
                "Windows would not let Disc Xplorer read this drive directly ({device}). \
                 Reading a disc Windows cannot mount needs elevated rights: try running \
                 Disc Xplorer as administrator."
            ),
            _ => format!("Cannot read this drive directly ({device}): {e}"),
        })?;

    // SetFilePointerEx reports the volume size on a device handle, which is what
    // Seek::End maps to. Without a length the tail of the disc cannot be read.
    let len = file
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("Cannot measure {device}: {e}"))?;
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("Cannot rewind {device}: {e}"))?;
    if len == 0 {
        return Err(format!("{device} reports a length of zero"));
    }
    Ok(AlignedReader::new(file, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A backing store that refuses anything a Windows volume handle would:
    /// reads have to start on a window boundary and ask for whole sectors.
    struct StrictDevice {
        data: Cursor<Vec<u8>>,
    }

    impl Read for StrictDevice {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let at = self.data.position();
            if at % WINDOW != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unaligned read at {at}"),
                ));
            }
            if buf.len() % 2048 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("partial sector read of {}", buf.len()),
                ));
            }
            self.data.read(buf)
        }
    }

    impl Seek for StrictDevice {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.data.seek(from)
        }
    }

    fn device(len: usize) -> AlignedReader<StrictDevice> {
        // Each byte encodes its own offset, so a misplaced read is visible.
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        AlignedReader::new(StrictDevice { data: Cursor::new(data) }, len as u64)
    }

    fn expect(at: u64, n: usize) -> Vec<u8> {
        (at..at + n as u64).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn an_unaligned_read_of_a_few_bytes_works() {
        let mut r = device(WINDOW as usize * 4);
        r.seek(SeekFrom::Start(6)).unwrap();
        let mut buf = [0u8; 6];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(buf.to_vec(), expect(6, 6));
    }

    /// The case the whole wrapper exists for: a read starting mid-sector and
    /// running across a window boundary.
    #[test]
    fn a_read_spanning_windows_is_reassembled() {
        let mut r = device(WINDOW as usize * 4);
        let at = WINDOW - 100;
        r.seek(SeekFrom::Start(at)).unwrap();
        let mut buf = vec![0u8; 500];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(buf, expect(at, 500));
    }

    #[test]
    fn reading_the_tail_stops_at_the_end() {
        let len = WINDOW as usize * 2 + 4096;
        let mut r = device(len);
        r.seek(SeekFrom::Start(len as u64 - 10)).unwrap();
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, expect(len as u64 - 10, 10));

        // And past the end reads nothing rather than failing.
        r.seek(SeekFrom::Start(len as u64 + 50)).unwrap();
        assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0);
    }

    #[test]
    fn seeking_from_end_and_current_lands_correctly() {
        let len = WINDOW as usize * 3;
        let mut r = device(len);
        assert_eq!(r.seek(SeekFrom::End(-2048)).unwrap(), len as u64 - 2048);
        assert_eq!(r.seek(SeekFrom::Current(-48)).unwrap(), len as u64 - 2096);
        let mut buf = [0u8; 8];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(buf.to_vec(), expect(len as u64 - 2096, 8));
        assert!(r.seek(SeekFrom::Start(0)).is_ok());
        assert!(r.seek(SeekFrom::Current(-1)).is_err());
    }

    /// Walking a directory means many small reads close together. Those must
    /// come out of one window rather than hitting the device each time.
    #[test]
    fn nearby_reads_reuse_one_window() {
        let mut r = device(WINDOW as usize * 2);
        for at in [0u64, 16, 2048, 4096, WINDOW - 1] {
            r.seek(SeekFrom::Start(at)).unwrap();
            let mut b = [0u8; 1];
            r.read_exact(&mut b).unwrap();
            assert_eq!(b[0], (at % 251) as u8);
        }
        assert_eq!(r.cache.as_ref().unwrap().0, 0, "should still be the first window");
    }
}
