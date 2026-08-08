### Batch Convert
A new **Tools → Batch Convert** window converts a folder of images in one pass: PS3 decrypt/encrypt, and Wii U `.wux`/`.wud` repackaged to ISO.

Nothing is written until you press Start. Before then it reports what would go wrong — images with no key, outputs that would replace an existing file, and whether the output volume has room. Existing files are handled by a policy you choose up front rather than a prompt part-way through. PS3 keys are matched by file name, or by the title ID inside an `.ird` when the names differ.

### 3DO
- **Directories are no longer truncated.** Entries can hold more than one copy, and directories longer than one block chain on by an index inside their own extent. Both were misread, so a prototype disc listed 27 of its 94 files and another listed 1 of 13. Any disc with a multi-block directory was affected.
- **Signed / Unsigned** now appears for 3DO discs, from a real RSA check against the retail key rather than a guess — covering the disc signature and every ROM tag payload. A disc carrying a signature that does not verify is reported as invalid.

### Wii and Wii U
- **Wii RVZ and WIA images now open.** They were refused before.
- **Korean and vWii discs decrypt correctly.** Only the retail common key was used, so those discs quietly produced noise rather than files.

### Nintendo DS
**`.nds` ROMs now open at all.** The check used to identify a cartridge was wrong, so every real ROM was rejected as unbrowsable.

### Also
- CHD images detect their filesystem when the caller does not name one, fixing sidebar expansion on 3DO CHDs.
- Sector View is always its own window; the setting is gone.
- Row select checkboxes are off by default, under **Settings → Select checkboxes**.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.8.4.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.8.4.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.8.4.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.8.4.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.8.4.AppImage` |
