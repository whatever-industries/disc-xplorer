### Accented Mac filenames are no longer read as Japanese

A French Mac disc showed `Système` as `Syst<kanji>e` and `Polices de caractères` as `Polices de caract<kanji>es`. HFS records no dependable encoding field, so Disc Xplorer works the encoding out from the names themselves — and that guess was too eager.

Accented Latin text in Mac OS Roman is *also* valid Shift-JIS. `Système` is `53 79 73 74 8F 6D 65`, and `8F 6D` is a legitimate two-byte kanji, so nothing looked wrong to the detector. `À` on its own is `0xCB`, which is halfwidth katakana. Detection now relies on full-width kana, whose lead bytes are rare in Western names, and Japanese discs are still recognised correctly.

**Settings → Mac filename encoding** lets you settle it yourself when the guess is still wrong: Auto, Mac OS Roman, or Shift-JIS. Changing it re-reads the disc straight away.

Thanks to @eingrossfilou for the report and the screenshot, which made the mis-decoding obvious.

### White window on Linux under Wayland

The AppImage could open to a blank white window, with `Could not create default EGL display: EGL_BAD_PARAMETER` on the terminal. WebKitGTK's DMABUF renderer needs EGL, and on a Wayland session where the AppImage's bundled `libwayland-client` disagrees with the host's, EGL refuses to start.

The renderer is now switched off on Linux, which avoids EGL entirely. If your setup worked before and you would rather keep it, export `WEBKIT_DISABLE_DMABUF_RENDERER=0`.

Reported by @eingrossfilou, who also found the `LD_PRELOAD` workaround that identified the cause.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.8.1.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.8.1.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.8.1.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.8.1.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.8.1.AppImage` |
