# TODO

- AaruFormat (.aif) — full browsing (complex multi-codec format; currently detect-only)
- Redumper DVD/BD dumps (.sdram/.sbram) — implemented but untested (no test files available)
- STFS (Xbox 360 CON/LIVE/PIRS packages) — not implemented; no legitimate sample available (they are game saves and DLC). Unlike the other containers, a misread produces plausible-looking garbage rather than a clean failure, because the format is hashed and block-chained, so it wants a real file to write against. Reference: SabreTools.Serialization STFS.cs (MIT), and "STFS Notes.txt" in github.com/Gualdimar/Velocity
- Format conversion, as the natural companion to the batch window: RVZ/WIA/GCZ/WBFS to ISO (GameCube, Wii), CSO/ZSO to and from ISO (PSP, PS2), CHD to CUE/BIN. Every one of these has a working reader already, so the work is a writer plus a job kind, not new format research. Deliberately not more encryption: the only targets left (3DS NCCH, Vita) need console-unique keys, unlike the published, disc-specific 3DO and PS3 ones.
