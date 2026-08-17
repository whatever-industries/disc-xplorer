### Converting between formats

**Batch Convert now has a target to convert to**, rather than inferring one. Auto still does what it always did: every image goes to its uncompressed form, and a PS3 ISO is decrypted or encrypted. The named targets force one format across a mixed folder.

| From | To |
|--------|-------|
| CSO / CISO, GCZ, WBFS, WUX / WUD, RVZ / WIA (GameCube) | ISO |
| ECM | BIN |
| ISO, IMG, WUD and the above | CSO (compressed ISO) |
| Wii U ISO / WUD | WUX (deduplicated, compressed) |
| CHD (CD) | CUE/BIN, either layout |

Conversions copy the disc contents through unchanged, so nothing is lost and no key is needed except for PS3.

**CUE/BIN sets can be merged and split.** A Redump dump with one BIN per track becomes a single BIN with a rewritten cue sheet, or the other way round, with the track files named to Redump's own convention. The cue sheet's own metadata survives the trip: REM lines, FLAGS, ISRC and session markers all carry across.

**CHD extraction.** A CD CHD becomes CUE/BIN, either as one BIN with the tracks indexed inside it or as one BIN per track.

Two things are deliberately refused rather than attempted, both because the result would look right and be unusable. Multi-track containers are not flattened into single-stream formats, since an ISO of a mixed-mode CD would silently drop its audio. And Wii RVZ and WIA are read but not converted to ISO: they store partitions decrypted, so rebuilding a raw image would need re-encryption.

### Batch Extract

**Point it at a folder and every image inside it is extracted into its own folder**, named after the disc's volume label rather than the file. Each disc is handled the way the single-disc **Extract All Contents** button handles it, so a hybrid disc gets one folder per filesystem and audio tracks land in an `Audio Tracks` folder beside the files.

Take files and audio, files only, or audio only. Audio-only turns a shelf of mixed-mode discs into a collection rip, with each disc's own CD-TEXT naming the tracks where it has any.

### Also

- Both batch windows take a drag-and-dropped folder, a single image, or a cue sheet.
- A **Clear** button empties a batch window when you change your mind.
- A finished batch says how it went in one line rather than a wall of text. **Copy log** still gives the full per-file record for a bug report.
- The conversion window follows the running job instead of leaving it to scroll off-screen.
- An existing BIN is never quietly replaced. Merging and splitting check every file they would write before writing any of them.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.9.0.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.9.0.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.9.0.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.9.0.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.9.0.AppImage` |
