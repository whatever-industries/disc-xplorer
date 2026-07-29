### Choose how CD-XA files are extracted
Right-clicking a CD-ROM XA streaming file (CD-i, Video CD, CD Extra, Saturn, PlayStation) now offers all three ways of pulling it off the disc, instead of one fixed choice in Settings:

- **Download** — the file's own content, each sector's user-data field. Video comes out as a clean, playable MPEG stream.
- **Download as XA** — keeps the 8-byte subheader and EDC, 2336 bytes/sector. XA-ADPCM audio needs the subheader's channel and coding bytes, and this matches what `dumpsxiso` produces.
- **Download raw** — whole 2352-byte sectors, sync and header included. This is what Windows hands back for a Form 2 file, so use it to byte-match a copy made through a CD drive, USBODE or an emulator.

The option appears only on files that actually are XA streaming files. This mirrors how IsoBuster splits the same choice across its extract menu.

### Version shown in the window title
The title bar now reads `Disc Xplorer  v1.5.1`, so it's clear at a glance which build is running. It's read from the app itself rather than typed in, so it can't drift from the actual release.

### Fixes
- Audio tracks: switching from one track to another reused the same player element, which WebKit doesn't always reload — each track now gets a fresh one
- `.bin` and `.img` are registered as openable disc images, so bin/cue dumps can be opened by double-click (opening a `.bin` still loads the whole disc via its cue sheet)

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.5.1.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.5.1.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.5.1.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.5.1.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.5.1.AppImage` |
