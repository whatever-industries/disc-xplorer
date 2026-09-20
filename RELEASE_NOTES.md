### Macintosh discs and CUE/BIN fixes

We've fixed detection of Macintosh discs that contain an HFS volume without an Apple partition map. **The Manhole** now opens as HFS instead of offering an ISO 9660 entry that fails to parse. Thanks to @bikerspade for the report.

We've also improved a few related cases:

- **CUE files moved from another machine:** old absolute paths and filename-case differences can now resolve to BINs beside the CUE. Ambiguous case matches are rejected, and later operations see files that have been added, renamed or removed. Batch planning remains fast for folders full of CUE sheets with missing BINs.
- **Images with subchannel data:** detection, the Sector Viewer and sector export now use the track's actual sector size, including 2448-byte sectors.
- **Busy drives on Windows:** a sharing violation no longer sends a readable disc down the raw-volume fallback intended for filesystems Windows cannot mount.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | [Disc.Xplorer_macOS_ARM_v1.9.6.zip](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.6/Disc.Xplorer_macOS_ARM_v1.9.6.zip) |
| **Windows** (x64) | [Disc.Xplorer_Windows_x64_v1.9.6.exe](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.6/Disc.Xplorer_Windows_x64_v1.9.6.exe) |
| **Windows** (ARM) | [Disc.Xplorer_Windows_ARM_v1.9.6.exe](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.6/Disc.Xplorer_Windows_ARM_v1.9.6.exe) |
| **Linux** (x64) | [Disc.Xplorer_Linux_x64_v1.9.6.AppImage](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.6/Disc.Xplorer_Linux_x64_v1.9.6.AppImage) |
| **Linux** (ARM) | [Disc.Xplorer_Linux_ARM_v1.9.6.AppImage](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.6/Disc.Xplorer_Linux_ARM_v1.9.6.AppImage) |
