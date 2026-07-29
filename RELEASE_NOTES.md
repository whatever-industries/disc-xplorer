### Accurate CD-XA (Mode 2) extraction
Streaming files on CD-i, Video CD, CD Extra, Saturn and PlayStation discs are stored with more content per sector than the directory record implies, and the amount depends on each individual sector. Disc Xplorer now reads that width from every sector instead of assuming one for the whole file, so extracted files match what's actually on the disc.

- **Form 2 sectors contribute 2324 bytes, Form 1 sectors 2048** — decided per sector, because a single file legitimately mixes both (a PlayStation `.XA` on our test disc is 155 Form 2 sectors interleaved with 5 Form 1)
- **CD Extra / Video CD video now extracts as a clean MPEG stream** — previously the subheader and EDC were written into the middle of it, changing the size and leaving 12 stray bytes per sector
- New **CD-XA extraction** setting: *File content* (default) is the above; *Keep subheader* writes a flat 2336 bytes/sector, which XA-ADPCM audio needs — its channel and coding bytes live in the subheader — and which matches what tools like dumpsxiso produce

Fixes #5.

### Japanese HFS discs
Filenames on Japanese Mac and hybrid discs are Shift-JIS, but plain HFS has no dependable field recording that, so they were being read as MacRoman and shown as mojibake. The encoding is now detected from the catalog itself.

### Open disc images from the desktop
Disc Xplorer now registers the disc-image file types it supports, so double-clicking one (or using "Open with") opens it directly instead of launching an empty window. Opening a second image while the app is running reuses the existing window rather than starting a new copy.

Generic extensions that other applications own — `.bin`, `.img`, `.raw`, `.toc`, `.aif`, `.zst` — are deliberately left unregistered; a `.cue` covers its `.bin` anyway. Fixes #4.

### Note on the Size column
For CD-XA streaming files the Size column is an estimate taken from the file's first sector, so a file that mixes Form 1 and Form 2 sectors can read slightly high or low — PlayStation `.STR` files typically read about 2% low. Extraction is always exact; only the displayed figure is approximate, and only in *File content* mode.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.5.0.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.5.0.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.5.0.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.5.0.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.5.0.AppImage` |
