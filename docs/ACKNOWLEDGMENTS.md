# Acknowledgments

## Audio encoder notices

Audio export uses two libraries, both statically linked at build time:

- **libFLAC** (Xiph.Org) for FLAC — BSD 3-Clause, https://xiph.org/flac/
- **LAME** for MP3 — GNU LGPL v2.1 or later, https://lame.sourceforge.io/


## Third-party references

Format documentation for several disc image and filesystem types was cross-referenced against the following projects. Except where noted below, the native Rust parsers in this project are independent implementations derived from the underlying format specifications; no third-party code is included.

**[FATXTools](https://github.com/aerosoul94/FATXTools)** — aerosoul94. The FATX / XTAF filesystem reader (`src-tauri/src/fatx_filesystem.rs`) is a Rust port of this project's FATX volume, directory-entry, and timestamp logic. Licensed MIT — Copyright (c) 2020 aerosoul94.

**[SabreTools.Serialization](https://github.com/SabreTools/SabreTools.Serialization)** — Matt Nadareski. Well-documented C# parsers for a wide range of disc image and filesystem formats. Licensed MIT — Copyright (c) 2018-2026 Matt Nadareski.

**[Aaru](https://github.com/aaru-dps/Aaru)** (`Aaru.Filesystems`, `Aaru.Images`) — Natalia Portillo. Comprehensive disc image preservation tool with format plugins for 70+ filesystem and 76+ image types. Licensed LGPL-2.1 (library components).

**[Wiimms ISO Tools](https://github.com/Wiimm/wiimms-iso-tools)** — Wiimm. Reference implementation for the WBFS container format (`libwbfs`). Licensed GPL-2.0.

**[MAME](https://github.com/mamedev/mame)** — MAMEDev and contributors. The CD CHD track layout (`src-tauri/src/chd_cue.rs`) follows the format defined in `src/lib/util/cdrom.cpp`, which chdman is built on. No MAME code is included; the track table is read from each file's own metadata. Referenced against MAME 0.289. Licensed GPL-2.0-or-later.

**[binmerge](https://github.com/putnam/binmerge)** — Chris Putnam. The CUE/BIN merge and split logic (`src-tauri/src/bincue.rs`) is a Rust port of this tool's sector arithmetic, cue sheet rewriting, and Redump track-naming rules. Licensed GPL-2.0-or-later — Copyright (C) 2024 Chris Putnam.

**[redumper](https://github.com/superg/redumper)** — superg and contributors. Accurate optical disc dumping tool (C2 error correction, subchannel preservation). Bundled as a sidecar binary; GPL-3.0 licensed. The redumper binary is not modified; it is downloaded from the official releases at build time and bundled for convenience, and is invoked as a separate process rather than linked.
