### A proper audio player
The player bar no longer borrows the browser's built-in audio controls, which looked and behaved differently on every platform. It now has its own transport:

- **⏮ / ⏭** move between tracks, in place of the 15-second seek buttons. ⏮ restarts the current track unless you're still near its start, the way a music player normally behaves; ⏭ stops being available on the last track. Data tracks are skipped.
- **▶ / ⏸**, a draggable seek bar, and elapsed / total time.
- **A volume slider** that stays put between tracks and between sessions.
- **🔁 continuous play** — when a track ends the next one follows automatically. On by default; turn it off and playback stops at the end of the current track.

The playback-speed control is gone, as it has no use on a disc of songs.

### Discs are named by the disc
The sidebar's top entry now shows the disc's own volume label rather than the image file's name, and extracting to a folder uses that label too — so a disc mastered as `TOKI_MIDI` extracts into `TOKI_MIDI/` instead of whatever the `.cue` happened to be called. The file name is still shown in the path bar and on hover. Discs with no volume label are unchanged.

On a hybrid disc the name follows the filesystem you're viewing, because the disc genuinely carries two: an HFS name and an ISO 9660 one.

### Sector View compare fits on screen
Comparing two images side by side no longer clips the right-hand bytes of every row. The two hex panels have room for a full 16-byte line, and a detached Sector View window opens — or grows — wide enough for both when compare is switched on. A window you've sized yourself is left alone.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.6.0.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.6.0.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.6.0.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.6.0.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.6.0.AppImage` |
