# TODO

- AaruFormat (.aif) — full browsing (complex multi-codec format; currently detect-only)
- Redumper DVD/BD dumps (.sdram/.sbram) — implemented but untested (no test files available)
- STFS (Xbox 360 CON/LIVE/PIRS packages) — not implemented; no legitimate sample available (they are game saves and DLC). Unlike the other containers, a misread produces plausible-looking garbage rather than a clean failure, because the format is hashed and block-chained, so it wants a real file to write against. Reference: SabreTools.Serialization STFS.cs (MIT), and "STFS Notes.txt" in github.com/Gualdimar/Velocity
- Nitro (.nds) — implemented but tested against a synthesised ROM only; no cartridge dump available
