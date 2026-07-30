### Gap handling, the way a ripper should do it
Every CD track can be preceded by a **gap** — a short lead-in the disc marks before the audio starts. It's usually silence, but not always: some albums hide an intro there, and live recordings often run straight through it.

Disc Xplorer now lets you say where that audio goes, using the same three choices, the same names, and the same default as **Exact Audio Copy**, under **Settings → Gap handling**:

- **Append gaps to previous track** *(default)* — the gap is written at the end of the track before it. Nothing is discarded.
- **Append gaps to next track** — the gap goes onto the track it introduces, which is what you want when a disc hides an intro there.
- **Leave out gaps** — gap sectors aren't written at all. Tracks start clean, at the cost of that audio.

**This changes the default output for discs dumped as one BIN per track.** Previously those gaps were skipped and nothing picked them up, so roughly two seconds of audio per track was quietly lost — while the very same album dumped as a single shared BIN kept it. The two layouts now produce the same result, and the default no longer discards anything.

Boundaries are exact in every mode: tracks meet with no overlap and no missing sectors, so no audio is ever written into two files at once.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.7.0.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.7.0.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.7.0.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.7.0.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.7.0.AppImage` |
