### Seven new formats

**Compressed disc images**

- **RVZ / WIA** — Dolphin's compressed GameCube images, including RVZ "packing", where the disc's junk padding is thrown away and regenerated on read. Wii images are refused with a clear message rather than served incorrectly; that needs partition hash handling which isn't done yet.
- **GCZ** — Dolphin's older compressed GameCube/Wii format.
- **WUA** — Cemu's Wii U Archive, alongside the existing WUX/WUD support.

**Archives**, browsable and extractable exactly like a disc:

- **ZIP** — stored, deflate, bzip2, LZMA and Zstandard members, plus Zip64
- **TAR** — plain or wrapped in gzip, bzip2, xz or Zstandard
- **CAB** — Microsoft Cabinet, uncompressed and MSZIP
- **VPK** — Valve Pak, v1 and v2, including multi-part archives
- **NDS** — the Nitro filesystem inside a Nintendo DS ROM

Archives inside a disc image can be opened in place, so a ZIP sitting on a 1990s CD-ROM browses like any other folder.

### PS3 discs know their own name

**IRD files are now accepted as keys.** IRD is how PS3 disc keys are actually catalogued and distributed, so decryption no longer needs a bare `.dkey` — point it at the `.ird` and it takes the key from there. A sibling `.ird` is found automatically, ahead of `.dkey` and `.key`.

PS3 discs also often leave their volume label blank, so they showed no name and extracted into a folder named after the image file. The real title now comes from the disc's own `PARAM.SFO`, falling back to the title ID in `PS3_DISC.SFB`.

### Correction

The README credited SabreTools.Serialization as LGPL-2.1. It is **MIT** — that has been fixed. The FFmpeg notice was also wrong: audio export uses statically linked libFLAC and LAME, neither of which is FFmpeg, and both are now named properly.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.8.0.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.8.0.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.8.0.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.8.0.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.8.0.AppImage` |
