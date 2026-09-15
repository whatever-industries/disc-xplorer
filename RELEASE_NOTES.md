### Wii, GameCube and Xbox discs can be read from a drive on Windows

Windows has no filesystem driver for these discs, so it never gives them a drive letter that can be browsed, and **Open Disc from Drive** failed with "the volume does not contain a recognized file system". macOS has always sidestepped this by reading the drive's device node directly; Windows now does the same thing, reading the raw volume when the disc is one Windows cannot mount.

Discs Windows *can* mount are unaffected: they are read exactly as before.

This needs a drive capable of reading the disc in the first place, such as one flashed with OmniDrive firmware. Reported by **Yola-cola** in [#14](https://github.com/whatever-industries/disc-xplorer/issues/14).

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.9.4.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.9.4.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.9.4.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.9.4.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.9.4.AppImage` |
