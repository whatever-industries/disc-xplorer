### Discs are no longer mistaken for Wii discs

A disc with no GameCube or Wii header fell through to a last-resort check that read the Wii partition table without verifying any of it. On a disc that is not a Wii disc, those bytes are ordinary file data, and they read as a partition table with over a billion entries pointing somewhere into the image. The first run of zero bytes it found there looked like a game partition, so the disc was labelled **Wii GCM**.

A 37 GB PlayStation 4 kiosk disc turned up this way. It reads correctly now, as ISO 9660 with its Joliet and Path Table views.

The check still works without header magic, which is the point of it, but a partition now has to hold a real Wii ticket before it counts. Wii, GameCube, WBFS and RVZ images are unaffected.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.9.2.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.9.2.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.9.2.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.9.2.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.9.2.AppImage` |
