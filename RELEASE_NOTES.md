### Extract All Contents now covers the whole disc

**Audio tracks are extracted too.** A mixed-mode or Enhanced CD gave up its files and silently dropped its music, and a pure audio CD never offered the button at all — its tracks could be played but not extracted. Tracks now go to an `Audio Tracks` folder beside the disc's files, or to the disc folder itself when there are none, using the existing format and gap settings.

**Every distinct filesystem is extracted, not just one.** A Mac/PC hybrid silently yielded only one side; an Xbox DVD only the game partition or only the DVD-Video zone. Each filesystem now gets its own folder. Alternative views of the same tree are not duplicated — Joliet and Rock Ridge are read as ISO 9660's names, and a UDF-bridge DVD is taken once through UDF rather than twice.

**Selecting a filesystem in the sidebar extracts just that one.** At the disc, session or track level you get everything; inside a filesystem you get what you pointed at.

### Also

- Audio CDs no longer fail with "No data track found in CUE sheet". Having no filesystem is a valid answer for an audio disc, and it was being treated as a detection failure.
- The progress window names the track being ripped, so a long FLAC rip no longer looks stalled.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.8.5.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.8.5.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.8.5.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.8.5.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.8.5.AppImage` |
