### Safer extraction and batch jobs

We've fixed two issues that could affect where extracted files end up:

- **Conflicting filenames:** if two entries would become the same destination name, extraction now reports the conflict instead of silently replacing one file with another. This also catches case differences and conflicting folder names. The error identifies the entries so we can save them separately with different names.
- **Batch extraction and conversion:** changing sources, output folders or options now invalidates the previous plan immediately. Start stays disabled while scanning, and a late response from an older scan can no longer replace the current plan. Clear also cancels pending scan results.

We've added regression tests for both fixes, including checks that complete Macintosh and Nintendo DS directory extractions match individual file reads byte-for-byte.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | [Disc.Xplorer_macOS_ARM_v1.9.7.zip](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.7/Disc.Xplorer_macOS_ARM_v1.9.7.zip) |
| **Windows** (x64) | [Disc.Xplorer_Windows_x64_v1.9.7.exe](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.7/Disc.Xplorer_Windows_x64_v1.9.7.exe) |
| **Windows** (ARM) | [Disc.Xplorer_Windows_ARM_v1.9.7.exe](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.7/Disc.Xplorer_Windows_ARM_v1.9.7.exe) |
| **Linux** (x64) | [Disc.Xplorer_Linux_x64_v1.9.7.AppImage](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.7/Disc.Xplorer_Linux_x64_v1.9.7.AppImage) |
| **Linux** (ARM) | [Disc.Xplorer_Linux_ARM_v1.9.7.AppImage](https://github.com/whatever-industries/disc-xplorer/releases/download/v1.9.7/Disc.Xplorer_Linux_ARM_v1.9.7.AppImage) |
