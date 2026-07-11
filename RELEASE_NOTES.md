### Bulk save
Tick boxes now sit next to every file's save arrow — select any mix of files and folders, then hit **Save Selected (n)** to extract them all into one destination. The header checkbox selects the whole folder.

### Timestamps preserved on extraction
Extracted files (and folders) now carry their **on-disc modified times** instead of the extraction time — single saves, bulk saves, folder saves, and Extract All, across every supported filesystem.

### Open any track file
Opening any `(Track N).bin` now finds its cue sheet and loads the **whole disc** — previously only the first data track happened to work, and without the track tree.

### Polish
- "Empty file" notice: line break, CLOSE button, and a persisted "Don't remind me again" checkbox; never interrupts folder extractions
- Zero-byte files show `0` in Size and `—` in LBA (their stored location is mastering filler)
- Sharper toolbar icons (SVG export glyph, square buttons), header/row save-column alignment
- Footer is no longer text-selectable; clicking the version number opens the releases page
- Column-resize handles no longer flash when mousing between headers

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.3.1.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.3.1.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.3.1.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.3.1.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.3.1.AppImage` |
