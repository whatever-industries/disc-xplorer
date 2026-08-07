- **Linux: fixes the white window under Wayland.** The AppImage now prefers the host's `libwayland-client` over its own bundled copy; where the two disagreed, EGL could not start and the app aborted before any window appeared.
- **Accented Mac filenames beginning with É or Ç are no longer read as Japanese.** Those are the Shift-JIS lead bytes for kana, so `École` decoded as katakana. Detection now needs two kana in a name, which no Western name produces.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.8.3.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.8.3.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.8.3.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.8.3.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.8.3.AppImage` |
