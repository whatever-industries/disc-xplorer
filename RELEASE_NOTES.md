### Extract from Xbox & Xbox 360 hard-drive images
Disc Xplorer now reads **FATX / XTAF**, the filesystem used on Xbox and Xbox 360 storage — so you can browse and extract files straight out of dev-drive and HDD images.

- **Both consoles, auto-detected** — original Xbox (little-endian `FATX`) and Xbox 360 (big-endian `XTAF`) are recognized automatically from the volume signature; no need to pick a mode.
- **Whole-drive or single-partition** — full original-Xbox HDD images expose each partition (Cache X/Y/Z, System, Data) as its own folder, while single-partition dumps mount directly at the root.
- **File timestamps** — last-modified dates are decoded and shown in the listing.
- Opens `.img`, `.bin`, `.fatx`, and extensionless raw dumps (alongside the existing `.iso`).

### Other
- Internal cleanup and build fixes

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v0.9.7.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v0.9.7.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v0.9.7.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v0.9.7.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v0.9.7.AppImage` |
