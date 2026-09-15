### Reading a disc Windows cannot mount now says what went wrong

1.9.4 added a way to read Wii, GameCube and Xbox discs directly from a drive on Windows, since Windows itself has no filesystem driver for them. When that path failed it quietly fell back to the old one, so the original "the volume does not contain a recognized file system" appeared again and said nothing useful.

Whatever goes wrong now reaches the screen. If Windows refuses direct access to the drive, the app says so and suggests running it as administrator, which is the usual reason.

It also decides when to take that path by whether the drive can actually be listed, rather than by whether Windows calls it a folder. Windows will happily call a drive a folder and then refuse to read it, which meant some of the discs this was written for never reached the new code at all.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.9.5.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.9.5.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.9.5.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.9.5.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.9.5.AppImage` |
