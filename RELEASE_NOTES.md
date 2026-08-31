### Windows file associations are now a choice

The installer used to associate every disc image type with Disc Xplorer without asking, which also repainted those files with our icon in Explorer. It now asks, and answering No leaves your file types exactly as they were, icons included. Thanks to **bikerspade** for reporting it ([#13](https://github.com/whatever-industries/disc-xplorer/issues/13)).

Nothing changes on macOS or Linux: neither ever claimed your file types. The macOS app supplies no document icon and never overrides a handler you have chosen, and the Linux AppImage installs nothing at all.

### ZSO

**ZSO images open, browse and extract like any other disc image**, and Batch Convert can write them. ZSO is the LZ4-compressed cousin of CSO, used on PSP and PS2: a little larger than CSO, noticeably quicker to read. Converting to it is under **Convert to** in Batch Convert, alongside CSO.

### Also

- The repository now carries a LICENSE file. Disc Xplorer is, and has always been, GPL v3.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.9.1.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.9.1.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.9.1.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.9.1.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.9.1.AppImage` |
