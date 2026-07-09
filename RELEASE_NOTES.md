### Fix: macOS app crashed at launch on machines without Homebrew
If Disc Xplorer quit immediately with "quit unexpectedly" on your Mac — this release fixes it. **No extra downloads or dependencies are needed; just replace the app.**

The app was accidentally linked against Homebrew's FLAC library, so it only started on machines that happened to have `brew install flac`. FLAC is now built in statically on every platform, and the app has zero external library dependencies.

---

## Download

| Platform | File |
|----------|------|
| **macOS** (Apple Silicon) | `Disc.Xplorer_macOS_ARM_v1.2.1.zip` |
| **Windows** (x64) | `Disc.Xplorer_Windows_x64_v1.2.1.exe` |
| **Windows** (ARM) | `Disc.Xplorer_Windows_ARM_v1.2.1.exe` |
| **Linux** (x64) | `Disc.Xplorer_Linux_x64_v1.2.1.AppImage` |
| **Linux** (ARM) | `Disc.Xplorer_Linux_ARM_v1.2.1.AppImage` |
