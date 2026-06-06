### Browse every filesystem on a disc
ISO 9660 discs now expose each filesystem and overlay as its own node in the tree — the way IsoBuster does — so you can see exactly what's present under each one:

- **Joliet** — long Windows-style filenames
- **Rock Ridge** — long POSIX filenames on Linux/Unix discs (instead of truncated `8.3` names)
- **El Torito** — boot images on bootable CDs/DVDs, which you can now browse and extract
- **Path Table** — the disc's directory index, for inspecting structure

### Better DVD detection
Hybrid **UDF-bridge** discs (most video and data DVDs carry both UDF *and* ISO 9660) now show both filesystems instead of collapsing to UDF only — so the ISO 9660 side is browsable again.

### Other
- Updated the bundled **redumper** dumper to build b722
- Internal cleanup and build fixes

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v0.9.6.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v0.9.6.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v0.9.6.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v0.9.6.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v0.9.6.AppImage` |

> **Windows users:** The `.updater.zip` files are used internally by the in-app auto-updater; you do not need to download them manually.
