# TODO

- AaruFormat (.aif) — full browsing (complex multi-codec format; currently detect-only)
- Redumper DVD/BD dumps (.sdram/.sbram) — implemented but untested (no test files available)
- STFS (Xbox 360 CON/LIVE/PIRS packages) — not implemented; no legitimate sample available (they are game saves and DLC). Unlike the other containers, a misread produces plausible-looking garbage rather than a clean failure, because the format is hashed and block-chained, so it wants a real file to write against. Reference: SabreTools.Serialization STFS.cs (MIT), and "STFS Notes.txt" in github.com/Gualdimar/Velocity
- Nitro (.nds, Nintendo DS) — implemented but tested against a synthesised ROM only; no cartridge dump available
- Wii RVZ/WIA — refused with a message; only GameCube images are read. Wii discs store their partitions in groups carrying hash "exception lists" that have to be replayed to rebuild each 0x8000 sector, and reading the raw data entries alone would hand back zeroes for every partition, so it fails loudly instead. Needs a Wii .rvz to write against. See the `disc_type == 2` guard in `wia_reader.rs` and Dolphin's docs/WiaAndRvz.md
