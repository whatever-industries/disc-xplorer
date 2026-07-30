### Consistent handling of CD-XA files, everywhere
CD-ROM XA streaming files (CD-i, Video CD, CD Extra, Saturn, PlayStation) can be written three different ways, and there's no single right answer. Disc Xplorer now asks once, remembers your answer if you want it to, and applies it to every way of getting files off a disc:

- The **save arrow** on a row
- **Save Selected**, for a batch of ticked files and folders
- **Download** from a right-clicked folder or filesystem in the sidebar
- **Extract All Contents**

The prompt appears only when what you're extracting actually contains CD-XA files, and offers:

- **File content** — each sector's user data; video comes out as a playable MPEG stream
- **Keep subheader** — a flat 2336 bytes/sector, which XA-ADPCM audio needs and which matches `dumpsxiso`
- **Raw sectors** — whole 2352-byte sectors, matching what Windows returns for a Form 2 file

Tick **Remember this choice** and it won't ask again. The setting lives under **Settings → CD-XA extraction**, where it can be changed or set back to asking. Right-clicking a single file still offers all three directly, so one file can be pulled a different way without touching the setting.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.5.3.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.5.3.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.5.3.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.5.3.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.5.3.AppImage` |
