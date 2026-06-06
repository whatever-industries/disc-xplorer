### Filesystem views
- ISO 9660 discs now break out each name space / overlay as its own browsable node, like IsoBuster:
  - **Joliet** — UCS-2 long filenames
  - **Rock Ridge** — POSIX long names (SUSP/RRIP `NM`, with `CE` continuations)
  - **El Torito** — bootable-CD boot images, listable and extractable
  - **Path Table** — diagnostic directory index
- **UDF-bridge** discs now show ISO 9660 alongside UDF instead of collapsing to UDF only

### Other
- Updated bundled `redumper` to build b722

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
