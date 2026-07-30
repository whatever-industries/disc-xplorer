### Audio tracks after the first now play
On a disc dumped as a single BIN — most CD-DA, mixed-mode and CD Extra images — only the first audio track produced sound; every other track decoded to an empty file and sat at 00:00. The track length was being derived with a formula that only holds when each track has its own BIN, and on a shared BIN it came out as zero for anything past track 1.

Both dump layouts are now handled, so every track plays and extracts in full. Discs with one BIN per track are unaffected — their output is byte-for-byte identical to before.

### "Extract All Contents" asks about CD-XA files
When a disc contains CD-ROM XA streaming files (audio or video), Extract All now asks how to write them instead of quietly choosing:

- **File content** — each sector's user data; video comes out as a playable MPEG stream
- **Keep subheader** — a flat 2336 bytes/sector, which XA-ADPCM audio needs and which matches `dumpsxiso`
- **Raw sectors** — whole 2352-byte sectors, matching what Windows returns for a Form 2 file

Discs with no XA files are unaffected and extract straight away, as before. The same three choices remain available per file from the right-click menu.

### Fixes
- The version in the status bar was white at reduced opacity over the light end of the gradient, which made it easy to miss entirely — it's now semibold at full opacity, and still links to the release notes

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.5.2.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.5.2.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.5.2.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.5.2.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.5.2.AppImage` |
