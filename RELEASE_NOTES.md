### Linux fixes

**The white window on Fedora Atomic is fixed** ([#10](https://github.com/whatever-industries/disc-xplorer/issues/10)). The Wayland workaround added in 1.8.3 preloaded the first `libwayland-client` it found, which on multilib layouts is the 32-bit one — so a 64-bit build preloaded a library the loader then refused. It now matches word size before preloading.

**Opening a file no longer kills the window** ([#11](https://github.com/whatever-industries/disc-xplorer/issues/11)). Every file listing drew its icons with the system colour-emoji font, and rendering one crashes WebKitGTK's Skia backend on Fedora 44. The icons are drawn by the app now, so nothing depends on the host's emoji font — and they look the same on every platform.

Both were reported with the diagnosis attached by **SkyNinja**, which is the only reason they were found and fixed this quickly.

### CD-TEXT

Discs that carry CD-TEXT now show their real track names instead of `Track 01`, and ripped files are named `03 - Eat for Two`. Read from a cue sheet's `CDTEXTFILE` or its inline `TITLE`/`PERFORMER` lines, and from a drive on macOS. Reading it from a drive on Linux and Windows is not implemented yet.

### Also

- Small grey text — sidebar labels, column headers, Settings headings — was below the contrast threshold at its size in both themes, and is now legible.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.8.6.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.8.6.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.8.6.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.8.6.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.8.6.AppImage` |
