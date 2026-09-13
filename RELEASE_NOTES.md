### AaruFormat images are recognised properly

Aaru writes `.aaruf`, and wrote `.dicf` under its former name DiscImageChef. We only ever looked for `.aif`, which Aaru has never written and which is really an audio extension, so an Aaru image went unrecognised: it was reported as an ISO 9660 disc and then failed the moment you tried to browse it.

Aaru images are now named for what they are. Reading them is still to come, but the app no longer claims a disc it cannot open.

The same fix covers every unknown file. Anything unrecognisable used to be labelled ISO 9660 on the assumption it was a raw disc image; a container that identifies itself is now named instead.

### Toolbar

The **Unmount Disc** and eject buttons have been tidied up. The red label was sitting on a blue button, the eject icon was drawn hollow by the system font at the wrong size, the two buttons were flush against each other while every other pair had a gap, and the eject button was three pixels shorter than its neighbours. All four are fixed.

### Also

- The window opens slightly shorter by default.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.9.3.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.9.3.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.9.3.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.9.3.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.9.3.AppImage` |
