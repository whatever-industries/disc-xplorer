### Zero-byte files, handled honestly
Some discs carry legitimate zero-byte files (timestamp markers, placeholders), and mastering tools often leave junk in their on-disc location fields. Disc Xplorer now treats them properly:

- **No more false damage flags** — a file with 0 bytes has nothing to read, so it can never earn the red-X.
- **"Empty file" notice** — saving one pops a short explanation that the empty result is how the disc was mastered, not a failed download. Includes a "Don't remind me again" option; folder extractions are never interrupted.
- **Cleaner columns** — Size shows a real `0` (instead of `—`), and the LBA column shows `—` since the stored value for a zero-byte file is meaningless filler.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.3.0.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.3.0.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.3.0.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.3.0.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.3.0.AppImage` |
